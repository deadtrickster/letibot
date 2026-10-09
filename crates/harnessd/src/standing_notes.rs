//! Standing notes: markdown files read into the system prompt.
//!
//! The operator's ask, 2026-10-08: *"we need memory concept. or rather notes.
//! something that will be injectable to compacted context verbatim up to
//! certain size and above it - summarized with references. i think markdown
//! files are fine. also make sure AGENTS.md reread after each compaction"*.
//!
//! # What a standing note is
//!
//! A file on disk maintained between sessions and expected to be read *now* —
//! not a memory of a past conversation. Two authorships, kept tellable apart
//! by directory: the operator's (`AGENTS.md` and the box-wide notes) and,
//! since the `notes` tool, the project's `.letibot/notes/`, which sessions
//! write too. The envelope says which is which, because authorship that
//! cannot be told apart is how a note the model wrote comes to read as an
//! instruction the operator gave. Three sources, in order of increasing
//! proximity to the task:
//!
//! 1. `<config dir>/notes/*.md` — standing notes for the whole box, beside
//!    `providers.toml` and `prompts.toml`.
//! 2. `<workspace>/AGENTS.md` — this project's instructions.
//! 3. `<workspace>/.letibot/notes/*.md` — this project's notes.
//!
//! The word "notes" is already spent in this crate: `Harness::open_notes` is
//! the resume side's observations about a session it did not resume, and the
//! TUI broadcasts head notices under the same name. This module is
//! **standing** notes — files that stand on disk and are read into the
//! prompt — and nothing here shares state with either of those.
//!
//! # When they are read — the constraint that decides it
//!
//! Injecting into history invalidates everything after the injection:
//! `docs/compaction.md` §2/§3 measure it at 144.6 s to re-prefill 150k tokens
//! against 0.8 s for a cache hit. So the cached prefix is never touched
//! mid-session on a local model, and the read happens at exactly the moments
//! the head of the prompt is being built anyway:
//!
//! * **Session open** — `Config::compose_system_with_notes`, before message 0
//!   is written.
//! * **Every base rebuild** — `Harness::reseat_target` re-reads the files
//!   before computing the next prefix, so a compaction (and `/reseat`,
//!   `/reingest`, which fork through the same seam) lands an edit to
//!   `AGENTS.md` in the new base's message 0. A compaction is a cold prefill
//!   by construction; changing the head there costs nothing extra.
//! * **Mid-session, provider sessions only** — `Harness::submit_item`
//!   re-reads the files and, when the assembled section differs from what
//!   the model was last given, appends it as a `System { origin: Update }`
//!   item. The operator's ruling, 2026-10-08: *"we cant touch prefix for
//!   local models only, for remote we can"* — a provider session has no
//!   local KV cache to protect, and the item is in the ledger, so what the
//!   transcript holds and what the model read are the same bytes. A local
//!   session gets nothing here and waits for the next base rebuild.
//!
//! # The size rule
//!
//! The assembled block is measured **in tokens with the session's own
//! counter** (`Vocab::tokenize_text` — the same encoder the ledger counts
//! with), not by a bytes-to-tokens estimate. At or under
//! [`NOTES_BUDGET_TOKENS`] the files are injected VERBATIM, headings and all;
//! over it, each file becomes a digest: its path, every heading with the
//! line range it spans, and the first sentence or two under it — enough for
//! the model to `read` the exact span for the rest. Deterministic, no model
//! call, no failure mode. If the counter itself refuses, the block is
//! treated as over budget: the digest is bounded by construction and the
//! verbatim form is not.
//!
//! # The envelope
//!
//! The whole block is wrapped in markers, for two unrelated reasons.
//!
//! *Structural distinguishability*: `docs/memory.md` §5 measures that
//! recalled material reading as something the model already concluded is
//! worse than silence. The opening sentence says what this is and who wrote
//! it, in a shape nothing else in the prompt produces.
//!
//! *Addressability*: [`replace`] and [`carried`] swap and read the section
//! by marker, so a re-read replaces the previous section wherever it sits in
//! a composed prompt — including one that `Sessions::with_fabric` has
//! already appended a fabric block after — without recomposing anything
//! else. A file that itself spells `[standing-notes-end]` will truncate the
//! section at that point on the next re-read; the pathological case is
//! stated rather than defended against.

use std::path::{Path, PathBuf};

use letibot_tokencore::Vocab;

/// How many tokens of standing notes are injected verbatim.
///
/// The operator asked for *"verbatim up to certain size and above it -
/// summarized with references"* without naming the size. Chosen without a
/// direct measurement, and anchored instead to the smallest number this tree
/// already records for a discretionary prompt budget: the compaction tail's
/// `clamp(2_000, 15_000, usable / 4)` floor (opencode's own, re-derived in
/// our units in `docs/compaction.md` §6). 2,000 tokens is roughly 8 KB of
/// markdown — a real `AGENTS.md` fits verbatim, and a corpus that does not
/// gets the digest with line references. Raise it with a measurement, not a
/// feeling.
pub const NOTES_BUDGET_TOKENS: usize = 2_000;

/// How many headings one file may contribute to a digest.
///
/// The digest is bounded by construction only if this exists: a file of
/// ten thousand headings would otherwise produce a ten-thousand-row digest
/// and the budget would have moved rather than been enforced. Past the cap
/// the file says so and stops.
const MAX_DIGEST_HEADINGS: usize = 200;

const BEGIN: &str = "[standing-notes-begin]";
const END: &str = "[standing-notes-end]";

/// The directory of box-wide standing notes: the config dir's `notes/`,
/// beside `providers.toml` and `prompts.toml` — the same rule
/// `Prompts::path` already uses for `prompts.toml`.
pub fn global_dir() -> PathBuf {
    letibot_provider::keys::config_file()
        .parent()
        .map(|p| p.join("notes"))
        .unwrap_or_else(|| PathBuf::from("notes"))
}

/// Read every source that exists, in order.
///
/// Later sources sit closer to the task in the block. Within a directory the
/// `*.md` glob is sorted by name so the block is a function of the files
/// alone — a prompt that varied with directory order is a prompt that misses
/// the prefix cache for no reason. A source that is absent, unreadable or
/// not valid UTF-8 contributes nothing; a note the operator cannot write as
/// UTF-8 is not a note.
fn gather(workspace: &Path, global: &Path) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    let md_in = |dir: &Path, out: &mut Vec<(PathBuf, String)>| {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut paths: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "md"))
            .collect();
        paths.sort();
        for p in paths {
            if let Ok(text) = std::fs::read_to_string(&p) {
                out.push((p, text));
            }
        }
    };
    md_in(global, &mut out);
    if let Ok(text) = std::fs::read_to_string(workspace.join("AGENTS.md")) {
        out.push((workspace.join("AGENTS.md"), text));
    }
    md_in(&workspace.join(".letibot").join("notes"), &mut out);
    out
}

/// The assembled standing-notes section, or `None` when no source exists.
///
/// One section for all sources together: they are one thing to the model
/// (standing material read from disk, whoever wrote it) and one thing to
/// [`replace`].
pub fn section(workspace: &Path, global: &Path, vocab: &Vocab) -> Option<String> {
    let files = gather(workspace, global);
    if files.is_empty() {
        return None;
    }
    let verbatim = verbatim_block(&files);
    // The session's own counter, not an estimate: the ledger is counted with
    // this encoder and a budget in another encoder's tokens is not a budget.
    // A refusal (no encoder can read this text) falls to the digest, which is
    // bounded by construction.
    let fits = vocab
        .tokenize_text(&verbatim)
        .map(|t| t.len() <= NOTES_BUDGET_TOKENS)
        .unwrap_or(false);
    let body = if fits { verbatim } else { digest_block(&files) };
    Some(format!(
        "{BEGIN}\nThe block below is standing notes, read from markdown files on disk by \
         the harness: the operator's standing material — AGENTS.md and the box-wide \
         notes — and this project's `.letibot/notes/`, which the `notes` tool writes \
         and the operator may write too. It is not text from this conversation. Treat \
         the newest copy you were given — in the system prompt or a later system \
         update — as the one in force. A note is historical: it records what was \
         true or intended when it was written and may be outdated, so follow its \
         instructions but check the tree before taking an observation in it as a \
         fact about now.\n\n{body}\n{END}"
    ))
}

/// Every file whole, under its path as a heading.
fn verbatim_block(files: &[(PathBuf, String)]) -> String {
    files
        .iter()
        .map(|(p, text)| format!("### {}\n{}", p.display(), text.trim_end()))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Every file as a path, per-heading line ranges, and the first sentence or
/// two under each heading — the references a `read` needs to fetch the rest.
fn digest_block(files: &[(PathBuf, String)]) -> String {
    let mut out = String::new();
    for (p, text) in files {
        let lines: Vec<&str> = text.lines().collect();
        out.push_str(&format!(
            "### {} — {} line(s), over the {}-token notes budget, so headings and line \
             ranges only. `read` this path with a range for the full text.\n",
            p.display(),
            lines.len(),
            NOTES_BUDGET_TOKENS
        ));
        let mut headings = 0usize;
        for (i, line) in lines.iter().enumerate() {
            let rest = line.trim_start();
            if !rest.starts_with('#') {
                continue;
            }
            headings += 1;
            if headings > MAX_DIGEST_HEADINGS {
                out.push_str(&format!(
                    "  … {} more heading(s) not listed\n",
                    lines[i..]
                        .iter()
                        .filter(|l| l.trim_start().starts_with('#'))
                        .count()
                        - 1
                ));
                break;
            }
            // The range this heading spans: its own line to the line before
            // the next heading, or the end of the file.
            let end = lines[i + 1..]
                .iter()
                .position(|l| l.trim_start().starts_with('#'))
                .map(|n| i + n)
                .unwrap_or(lines.len() - 1);
            let lead = sentences(&lines[i + 1..=end]);
            out.push_str(&format!(
                "  {}-{}  {}{}\n",
                i + 1,
                end + 1,
                rest,
                lead.map(|s| format!(" — {s}")).unwrap_or_default()
            ));
        }
        if headings == 0 {
            out.push_str("  (no headings; the first lines follow)\n");
            let lead = sentences(&lines).unwrap_or_default();
            out.push_str(&format!(
                "  1-{}  {}\n",
                lines.len(),
                lead.chars().take(200).collect::<String>()
            ));
        }
    }
    out.trim_end().to_string()
}

/// The first one or two sentences of a span: enough to recognise, short
/// enough that a digest stays a digest. Empty spans contribute nothing.
fn sentences<'a>(lines: &[&'a str]) -> Option<String> {
    let text: String = lines
        .iter()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect::<Vec<_>>()
        .join(" ");
    if text.is_empty() {
        return None;
    }
    let mut count = 0;
    let mut cut = text.len();
    for (i, c) in text.char_indices() {
        if c == '.' {
            count += 1;
            if count == 2 {
                cut = i + 1;
                break;
            }
        }
    }
    Some(text.chars().take(text[..cut].chars().count()).collect())
}

/// Swap the standing-notes section inside a composed system prompt.
///
/// Idempotent and position-independent: an existing section is replaced in
/// place wherever its markers sit, `None` removes it, and everything outside
/// the markers — the composed sections, `system_extra`, a fabric block
/// appended after — is carried through byte for byte. This is what lets a
/// re-read change only the notes without recomposing anything else.
pub fn replace(system: &str, section: Option<&str>) -> String {
    match (system.find(BEGIN), system.find(END)) {
        (Some(a), Some(b)) if b >= a => {
            let after = &system[b + END.len()..];
            let mut out = String::with_capacity(system.len());
            out.push_str(&system[..a]);
            if let Some(s) = section {
                out.push_str(s);
            }
            // Removing a section that was glued with "\n\n" must not leave a
            // doubled blank line where it was — the glue that joined the old
            // section to its neighbours is spent with the section, whichever
            // side of it the remaining prompt sits on.
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

/// The standing-notes section a composed system prompt currently carries,
/// markers included — `None` when it carries none. What the model was last
/// given, read back off the prompt itself rather than remembered beside it.
pub fn carried(system: &str) -> Option<&str> {
    let a = system.find(BEGIN)?;
    let b = system[a..].find(END)? + a + END.len();
    Some(&system[a..b])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The byte vocabulary: one token per byte, no GGUF, no FFI — the same
    /// counter a provider session counts with, so the budget arithmetic in
    /// these tests is exact rather than proportional.
    fn vocab() -> Vocab {
        Vocab::bytes([], [])
    }

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "letibot-standing-{}-{}-{name}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("temp dir");
        d
    }

    fn write(path: &Path, text: &str) {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).expect("parent");
        }
        std::fs::write(path, text).expect("fixture");
    }

    /// **Under the budget the files are whole, each under its path.**
    #[test]
    fn under_the_budget_the_files_are_injected_verbatim() {
        let ws = dir("verbatim");
        write(
            &ws.join("AGENTS.md"),
            "# Rules\n\nBuild with `cargo test -p`, never `--workspace`.\n",
        );
        let s = section(&ws, &dir("none"), &vocab()).expect("a section");
        assert!(s.contains(BEGIN) && s.contains(END), "{s}");
        assert!(
            s.contains(&format!("### {}/AGENTS.md", ws.display())),
            "the path is the heading: {s}"
        );
        assert!(
            s.contains("Build with `cargo test -p`, never `--workspace`."),
            "the file's own text, whole: {s}"
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    /// **Over the budget it is a digest with line references, not the text.**
    #[test]
    fn over_the_budget_it_is_a_digest_with_line_references() {
        let ws = dir("digest");
        let body: String = "filler sentence that exists only to spend the budget. ".repeat(80);
        let file = format!(
            "# Title\n\nFirst sentence under the title. Second sentence, rarely needed.\n\n## Deep\n\n{body}\n"
        );
        write(&ws.join("AGENTS.md"), &file);
        assert!(file.len() > NOTES_BUDGET_TOKENS, "the fixture must exceed");
        let s = section(&ws, &dir("none"), &vocab()).expect("a section");
        assert!(
            s.contains("digest with line references")
                || s.contains("headings and line ranges only"),
            "the digest says what it is: {s}"
        );
        // The deep body is absent — that is the whole point of the rule.
        assert!(!s.contains(&body), "the digest carries no filler body");
        // The references a `read` needs: heading, span, first sentences.
        // The title spans lines 1-4 (its own line to the line before the next
        // heading) and Deep spans 5-7 to the end of the file.
        assert!(s.contains("1-4  # Title"), "{s}");
        assert!(s.contains("First sentence under the title."), "{s}");
        assert!(
            s.contains("5-7  ## Deep"),
            "the Deep heading's span is addressed: {s}"
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    /// **A source that is absent contributes nothing — never a heading, never
    /// an error.**
    #[test]
    fn a_missing_source_contributes_nothing() {
        let empty = dir("empty");
        assert!(
            section(&empty, &dir("none"), &vocab()).is_none(),
            "no sources is no section"
        );
        let ws = dir("one");
        write(&ws.join("AGENTS.md"), "only file\n");
        let s = section(&ws, &dir("none"), &vocab()).expect("a section");
        assert_eq!(s.matches("### ").count(), 1, "one file, one heading: {s}");
        assert!(
            !s.contains("notes]"),
            "no empty heading for the absent dirs"
        );
        let _ = std::fs::remove_dir_all(&ws);
        let _ = std::fs::remove_dir_all(&empty);
    }

    /// **The order is global, then AGENTS.md, then the project's notes; and
    /// within a directory it is the sorted glob, not the directory's order.**
    #[test]
    fn the_sources_keep_their_order_and_sort_within_a_directory() {
        let ws = dir("order");
        let global = dir("global");
        write(&global.join("z-first-written.md"), "global\n");
        write(&ws.join("AGENTS.md"), "agents\n");
        write(&ws.join(".letibot/notes/b.md"), "project b\n");
        write(&ws.join(".letibot/notes/a.md"), "project a\n");
        let s = section(&ws, &global, &vocab()).expect("a section");
        let pos = |needle: &str| s.find(needle).expect(needle);
        assert!(
            pos("global\n") < pos("agents\n")
                && pos("agents\n") < pos("project a\n")
                && pos("project a\n") < pos("project b\n"),
            "global, then the project file, then the project's notes sorted: {s}"
        );
        let _ = std::fs::remove_dir_all(&ws);
        let _ = std::fs::remove_dir_all(&global);
    }

    /// **The same files assemble to the same section — twice.**
    #[test]
    fn the_same_files_assemble_to_the_same_section() {
        let ws = dir("same");
        write(&ws.join("AGENTS.md"), "# One\n\ntext\n");
        let a = section(&ws, &dir("none"), &vocab());
        let b = section(&ws, &dir("none"), &vocab());
        assert_eq!(a, b);
        let _ = std::fs::remove_dir_all(&ws);
    }

    /// **The budget's edge: at it verbatim, one over it a digest.**
    #[test]
    fn the_budget_edge_is_exact() {
        let ws = dir("edge");
        let path = ws.join("AGENTS.md");
        // One byte = one token under the byte vocabulary, and the budget is
        // decided on the assembled BODY (the path heading plus the files) —
        // the envelope is the harness's own fixed sentence, not the operator's
        // material, and budgeting it would spend the operator's allowance on
        // our wording. Build the file so the body sits exactly on the budget.
        let overhead = format!("### {}\n", path.display()).len();
        let file = "a".repeat(NOTES_BUDGET_TOKENS - overhead);
        write(&path, &file);
        let s = section(&ws, &dir("none"), &vocab()).unwrap();
        assert!(s.contains(&file), "exactly at the budget: verbatim");
        write(&path, &format!("{file}a"));
        let over = section(&ws, &dir("none"), &vocab()).unwrap();
        assert!(
            over.contains("headings and line ranges only"),
            "one token over: {over}"
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    /// **`replace` swaps the section and leaves the rest byte for byte.**
    #[test]
    fn replace_swaps_only_the_section() {
        let ws = dir("replace");
        write(&ws.join("AGENTS.md"), "first\n");
        let one = section(&ws, &dir("none"), &vocab()).unwrap();
        // A composed prompt as `with_fabric` leaves it: sections, notes, then
        // a fabric block appended after.
        let composed = format!("sections\n\n{one}\n\nfabric block");
        write(&ws.join("AGENTS.md"), "second\n");
        let two = section(&ws, &dir("none"), &vocab()).unwrap();
        let swapped = replace(&composed, Some(&two));
        assert!(swapped.contains("second"), "{swapped}");
        assert!(!swapped.contains("first"), "{swapped}");
        assert!(
            swapped.starts_with("sections\n") && swapped.ends_with("fabric block"),
            "everything outside the markers is carried through: {swapped}"
        );
        assert_eq!(carried(&swapped), Some(two.as_str()));
        // Removing: no doubled blank line where the section was, mid-prompt or
        // at the end — a removal from the end takes its glue with it.
        let removed = replace(&swapped, None);
        assert_eq!(removed, "sections\n\nfabric block");
        assert_eq!(
            replace(&format!("plain\n\n{one}"), None),
            "plain",
            "a removal from the end leaves no tail of blank lines"
        );
        // Idempotent on a prompt that never had one.
        assert_eq!(replace("plain", None), "plain");
        let _ = std::fs::remove_dir_all(&ws);
    }

    /// **`carried` reads the section back off a composed prompt.**
    #[test]
    fn carried_reads_the_section_off_a_composed_prompt() {
        let ws = dir("carried");
        write(&ws.join("AGENTS.md"), "text\n");
        let s = section(&ws, &dir("none"), &vocab()).unwrap();
        assert_eq!(carried(&format!("before\n\n{s}\nafter")), Some(s.as_str()));
        assert_eq!(carried("no section here"), None);
        let _ = std::fs::remove_dir_all(&ws);
    }

    /// **The envelope says whose notes these are, and that they are
    /// historical.**
    ///
    /// The operator's ruling, 2026-10-09: *"The read tool should carry a short
    /// note - 'it is a historical note, might be outdated'"* — and the injected
    /// block is the highest-traffic read of all, delivered at session open and
    /// every base rebuild whether or not the tool is ever called. The envelope
    /// also had to change for a second reason: the project's `.letibot/notes/`
    /// is now writable by sessions through the `notes` tool, so a blanket "the
    /// operator maintains these" stopped being true. The authorship split is
    /// by directory — the tell that keeps a note the model wrote from reading
    /// as an instruction the operator gave — and the caveat says what a note
    /// is: a record of what was true when written, not a fact about now.
    #[test]
    fn the_envelope_splits_authorship_and_carries_the_historical_caveat() {
        let ws = dir("envelope");
        write(
            &ws.join(".letibot/notes/session-find.md"),
            "a session wrote this\n",
        );
        let s = section(&ws, &dir("none"), &vocab()).unwrap();
        assert!(
            s.contains("the operator's standing material"),
            "whose AGENTS.md and box-wide notes are: {s}"
        );
        assert!(
            s.contains(".letibot/notes/`"),
            "the project notes are named as their own source, written by the tool: {s}"
        );
        assert!(
            s.contains("not text from this conversation"),
            "the structural distinguishability sentence survives: {s}"
        );
        assert!(
            s.contains("historical") && s.contains("may be outdated"),
            "the caveat, in the operator's own words: {s}"
        );
        assert!(
            s.contains("one in force"),
            "the which-copy-is-current rule survives: {s}"
        );
        let _ = std::fs::remove_dir_all(&ws);
    }
}
