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
//! instruction the operator gave.
//!
//! # The order, which is a rule
//!
//! Three sources, in the order the block carries them — and the order is a rule
//! and not a preference, because a file spends the budget by being whole (see
//! "The size rule" below), so the order decides what arrives whole:
//!
//! 1. `<workspace>/AGENTS.md` — this project's instructions.
//! 2. `<config dir>/notes/*.md` — standing notes for the whole box, beside
//!    `providers.toml` and `prompts.toml`.
//! 3. `<workspace>/.letibot/notes/*.md` — this project's notes.
//!
//! `AGENTS.md` first, because a rule that arrives as an index is a rule the
//! model has to fetch before it can follow it. Within a directory: **newest
//! first**, ties by name — a note written this session is the note about what is
//! happening now, and the file most likely to be worth its whole text. The sort
//! is [`letibot_tools::builtins::notes::sort_newest_first`], shared with the
//! `notes` tool's `list`, which claims to report the notes in prompt order; and
//! it is a sort rather than `readdir`'s order, because a block that varied with
//! directory order is a block that misses the prefix cache for no reason.
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
//! mid-session, and the read happens at exactly the moments the head of the
//! prompt is being built anyway — which are one schedule, because a reseat or
//! a compaction IS a re-open of that same head:
//!
//! * **Session open** — `Config::compose_system_with_notes`, before message 0
//!   is written.
//! * **Every base rebuild** — `Harness::reseat_target` re-reads the files
//!   before computing the next prefix, so a compaction (and `/reseat`,
//!   `/reingest`, which fork through the same seam) lands an edit to
//!   `AGENTS.md` in the new base's message 0. A compaction is a cold prefill
//!   by construction; changing the head there costs nothing extra.
//!
//! A per-turn delivery on provider sessions was added on 2026-10-08 and
//! removed the next day on the operator's ruling — *"look at the original
//! ask - session open, which covers reseats, compaction"* — after a note
//! arrived wearing a dialect's `<system-update>` envelope mid-conversation.
//! The same schedule serves every session; nothing re-reads the files between
//! these two moments.
//!
//! # The size rule, per file
//!
//! The assembled block is measured **in tokens with the session's own counter**
//! (`Vocab::tokenize_text` — the same encoder the ledger counts with), not by a
//! bytes-to-tokens estimate. Each file, in the order above, is injected VERBATIM
//! — headings and all — when it fits what is left of [`NOTES_BUDGET_TOKENS`];
//! when it does not, that file alone becomes an index: its path, its abstract,
//! every heading with the line range it spans and the first sentence under it —
//! enough for the model to `read` the exact span for the rest.
//!
//! **Per file, and that is the point.** Measuring the whole assembly at once and
//! digesting everything when it failed made a ten-line note pay for a
//! four-hundred-line one; now the small note is whole and only the large one is
//! indexed. The order above is what decides which is which, so it is stated
//! rather than left to whatever the filesystem answered. The index a file falls
//! back to is counted against the budget too — the budget bounds the section,
//! not the verbatim part of it — and an indexed file says on its own heading that
//! it is indexed.
//!
//! Deterministic, no model call, no failure mode. If the counter itself refuses a
//! file's text, that file is indexed rather than admitted on a guess: the index is
//! bounded by construction and the verbatim form is not.
//!
//! # The abstract, and a note with no headings
//!
//! Each note's index entry opens with its **abstract**: the author's own line
//! when the file carries one (the `notes` tool's `abstract` argument writes it),
//! and the first *proper* sentence of the note's prose otherwise. Both live in
//! [`letibot_tools::builtins::notes`], where the writer is — one derivation, so
//! the tool's report of what the index will say cannot disagree with the index.
//!
//! **Proper** is the whole of it: a period ends a sentence only when whitespace
//! or the end of the text follows it, so a lead never ends inside `exit 101 in
//! 2.7 s` or inside `merge_review.attempts` — the two cuts this rule was measured
//! making. A sentence longer than `ABSTRACT_CHARS` is cut at a character boundary
//! and says so with an ellipsis.
//!
//! A note with **no headings** is indexed by its paragraphs: every
//! blank-line-separated block gets its own line range and its own first sentence.
//! Before this, such a note over the budget arrived as one truncated line — a
//! note the model could neither recognise nor address, which is the one thing an
//! index must not be.
//!
//! # The offering half, which is not here
//!
//! The index is passive: it says what the notes are, and the model has to think
//! to look. The other half — a hint that a note *bears on what you are doing
//! now*, with a score, over this corpus — is deliberately not built here. Where
//! it would attach, and what it must not do:
//!
//! * **The corpus** is [`gather`]'s output: the same files, in the same order,
//!   read once. A scoring pass wants that list and nothing else, which is why it
//!   is public.
//! * **The delivery is a transcript row, never the prefix.** This module's whole
//!   schedule exists because injecting into history invalidates everything after
//!   it — `docs/compaction.md` §2/§3 measure 144.6 s to re-prefill 150k tokens
//!   against 0.8 s for a cache hit. A hint is OFFERED, not asserted: it goes in
//!   the way `Harness::wake` and `Harness::nag_turn` already let the harness talk
//!   to itself — `Harness::submit_item(TranscriptItem::User { speaker:
//!   Speaker::Agent, .. })`, a row after the prefix, which costs one turn and no
//!   cache.
//! * **Never a `read`.** A hint that silently spends the model's context on a
//!   note is the injected-material failure `docs/memory.md` §5 measures; the
//!   model decides whether to open what was offered.
//! * **Nothing here may be re-read mid-session.** A score computed at session
//!   open is stale by the second turn and there is no seam to refresh it through;
//!   a hint belongs to the turn it is offered in.
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
// **The writer's conventions, used by the reader.** The abstract's marker, its
// derivation and the order a notes directory is assembled in live with the
// `notes` tool, which reports to an author what the index will say: a second
// implementation here would be a report that can disagree with the index it
// describes. The reader imports them rather than restating them.
use letibot_tools::builtins::notes::{abstract_of, first_sentence, sort_newest_first};

/// How many tokens of standing notes are injected verbatim.
///
/// The operator asked for *"verbatim up to certain size and above it -
/// summarized with references"* without naming the size. Chosen without a
/// direct measurement, and anchored instead to the smallest number this tree
/// already records for a discretionary prompt budget: the compaction tail's
/// `clamp(2_000, 15_000, usable / 4)` floor (opencode's own, re-derived in
/// our units in `docs/compaction.md` §6). 2,000 tokens is roughly 8 KB of
/// markdown — a real `AGENTS.md` fits verbatim, and a corpus that does not
/// gets the index with line references. Raise it with a measurement, not a
/// feeling.
pub const NOTES_BUDGET_TOKENS: usize = 2_000;

/// How many headings one file may contribute to its index.
///
/// The index is bounded by construction only if this exists: a file of
/// ten thousand headings would otherwise produce a ten-thousand-row index
/// and the budget would have moved rather than been enforced. Past the cap
/// the file says so and stops.
const MAX_INDEX_HEADINGS: usize = 200;

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

/// Read every source that exists, in the order the section carries them.
///
/// **The order is a rule, and this is where it lives.** A file spends the budget
/// by being whole, so the order decides what arrives whole and what arrives as an
/// index — and the first source is the one that has to arrive whole, because a
/// rule the model has to fetch before it can follow it is a rule it may not
/// follow:
///
/// 1. `<workspace>/AGENTS.md` — the operator's instructions for this project.
/// 2. `<global>/*.md` — the box-wide notes.
/// 3. `<workspace>/.letibot/notes/*.md` — this project's notes.
///
/// Within a directory: **newest first**, ties by name
/// ([`letibot_tools::builtins::notes::sort_newest_first`], the same sort the
/// `notes` tool's `list` reports them in, so the two cannot disagree about what
/// follows what). Newest first because a note written this session is the note
/// about what is happening now — and a sort rather than `readdir`'s order, so the
/// block is a function of the files alone: a prompt that varied with directory
/// order is a prompt that misses the prefix cache for no reason.
///
/// Public because the offering half (see the module doc) wants exactly this list
/// — the corpus, in this order, read once. A source that is absent, unreadable or
/// not valid UTF-8 contributes nothing; a note the operator cannot write as UTF-8
/// is not a note.
pub fn gather(workspace: &Path, global: &Path) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    let agents = workspace.join("AGENTS.md");
    if let Ok(text) = std::fs::read_to_string(&agents) {
        out.push((agents, text));
    }
    out.extend(md_in(global));
    out.extend(md_in(&workspace.join(".letibot").join("notes")));
    out
}

/// Every `*.md` file in `dir`, newest first, read. A directory that is absent or
/// unreadable is no files rather than an error, and so is a file that is not
/// valid UTF-8.
fn md_in(dir: &Path) -> Vec<(PathBuf, String)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "md"))
        .collect();
    sort_newest_first(&mut paths);
    paths
        .into_iter()
        .filter_map(|p| std::fs::read_to_string(&p).ok().map(|text| (p, text)))
        .collect()
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
    Some(format!(
        "{BEGIN}\nThe block below is standing notes, read from markdown files on disk by \
         the harness: the operator's standing material — AGENTS.md and the box-wide \
         notes — and this project's `.letibot/notes/`, which the `notes` tool writes \
         and the operator may write too. It is not text from this conversation. Treat \
         the newest copy you were given — in the system prompt or a later system \
         update — as the one in force. A note is historical: it records what was \
         true or intended when it was written and may be outdated, so follow its \
         instructions but check the tree before taking an observation in it as a \
         fact about now. A file whose heading says `indexed` did not fit the budget: \
         what follows it is an index — line ranges a `read` can fetch — and not its \
         text.\n\n{}\n{END}",
        body(&files, vocab)
    ))
}

/// The body: each file whole if it fits what is left of the budget, indexed if it
/// does not — decided **per file**, in [`gather`]'s order.
///
/// The join between two entries is not counted. The budget is what the operator's
/// material costs; the two bytes that separate two files are the harness's own
/// glue, like the envelope's sentence.
fn body(files: &[(PathBuf, String)], vocab: &Vocab) -> String {
    let mut out = String::new();
    let mut used = 0usize;
    for (path, text) in files {
        let whole = format!("### {}\n{}", path.display(), text.trim_end());
        // The session's own counter, not an estimate: the ledger is counted with
        // this encoder and a budget in another encoder's tokens is not a budget.
        match tokens(vocab, &whole) {
            Some(n) if used + n <= NOTES_BUDGET_TOKENS => {
                used += n;
                push(&mut out, &whole);
            }
            // Either it does not fit, or the counter refused the text outright —
            // in which case the index is what this file gets, because the index is
            // bounded by construction and the verbatim form is not.
            _ => {
                let index = index_entry(path, text);
                used += tokens(vocab, &index).unwrap_or(0);
                push(&mut out, &index);
            }
        }
    }
    out
}

fn push(out: &mut String, entry: &str) {
    if !out.is_empty() {
        out.push_str("\n\n");
    }
    out.push_str(entry);
}

/// How many tokens the session's own counter makes of `text` — `None` when the
/// counter refuses it, which is a fact about the text and not a zero.
fn tokens(vocab: &Vocab, text: &str) -> Option<usize> {
    vocab.tokenize_text(text).ok().map(|t| t.len())
}

/// One file as an index: its path, its abstract, and then every heading with the
/// line range it spans and the first sentence under it — or, for a note with no
/// headings at all, every paragraph the same way. The references a `read` needs
/// to fetch the rest.
fn index_entry(path: &Path, text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = format!(
        "### {} — {} line(s), indexed: over what is left of the {}-token notes budget, so \
         headings and line ranges only. `read` this path with a range for the full text.\n",
        path.display(),
        lines.len(),
        NOTES_BUDGET_TOKENS
    );
    // The abstract, first, because it is the one line that says whether the rest
    // is worth fetching — and named as derived when it is, so a reader never
    // takes the harness's reading of a note for the author's own summary.
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
        // **A note with no headings is indexed by its paragraphs, never collapsed
        // to one line.** Squashing the whole note into a single truncated line —
        // which is what stood here — gives the model neither something it can
        // recognise nor a range it can `read`.
        for (first, last, lead) in paragraphs(&lines) {
            // A block with no prose in it — the abstract marker standing alone,
            // which is how the `notes` tool writes one — is not a paragraph and
            // says nothing as a row: the abstract above is already its content.
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
        // The range this heading spans: its own line to the line before the next
        // heading, or the end of the file.
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

/// The blank-line-separated blocks of a headingless note: `(first line, last
/// line, first sentence)`, 1-based and inclusive, in file order.
///
/// The ranges are the file's real line numbers, so a range a reader is given can
/// be handed straight to `read` — including a block that contains the abstract
/// marker, which carries no prose of its own but is a line like any other.
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

/// **The census, in its own file** — the shape `harness/queue_e2e.rs` has, and for
/// the same reason: these tests read this module's private rules (`gather`'s
/// order, `body`'s per-file budget, `index_entry`'s shape), and a module whose
/// rules are private can only be tested from inside it. The integration half —
/// the re-read at a base rebuild, against a real harness — is
/// `crates/harnessd/tests/standing_notes.rs`.
#[cfg(test)]
mod tests;
