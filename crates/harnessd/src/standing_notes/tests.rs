//! **The census: this module's rules, asserted.**
//!
//! In its own file, the shape `harness/queue_e2e.rs` has and for the same
//! reason: these tests read the module's *private* rules — `gather`'s order,
//! `body`'s per-file budget, `index_entry`'s shape, the sentence rule behind an
//! abstract — and a module whose rules are private can only be tested from
//! inside it. The declaration is `mod tests;` at the bottom of
//! `standing_notes.rs`; the integration half — the re-read at a base rebuild,
//! against a real harness — is `crates/harnessd/tests/standing_notes.rs`.
//!
//! What is asserted here, in one line each: the files whole under the budget and
//! each one indexed on its own when it does not fit; a small note whole beside a
//! large one, in either order; a headingless note indexed by its paragraphs
//! rather than collapsed to a line; the abstract a proper sentence, never cut
//! inside `2.7` or a path, and the author's own line when the file carries one;
//! the order `gather` states; the budget's edge exact; `replace` and `carried`
//! byte-for-byte around the section; and the envelope's authorship split and
//! historical caveat.

use super::*;

/// The byte vocabulary: one token per byte, no GGUF, no FFI — the same counter a
/// provider session counts with, so the budget arithmetic in these tests is exact
/// rather than proportional.
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

/// A file's mtime, **stated rather than waited for**. "Newest first" is a rule
/// this module asserts, and a sleep between two writes would make the assertion
/// depend on the clock's resolution instead of on the rule.
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

/// A note whose body alone is over the budget, for the tests that need one file
/// indexed and care about nothing else in it.
fn over_budget() -> String {
    "filler sentence that exists only to spend the budget. ".repeat(80)
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
    assert!(
        !s.contains(", indexed:"),
        "nothing is indexed when everything fits: {s}"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

/// **Over the budget a file is an index with line references, not its text** —
/// and it says on its own heading that it is indexed.
#[test]
fn over_the_budget_it_is_an_index_with_line_references() {
    let ws = dir("index");
    let body = over_budget();
    let file = format!(
        "# Title\n\nFirst sentence under the title. Second sentence, rarely needed.\n\n## Deep\n\n{body}\n"
    );
    write(&ws.join("AGENTS.md"), &file);
    assert!(file.len() > NOTES_BUDGET_TOKENS, "the fixture must exceed");
    let s = section(&ws, &dir("none"), &vocab()).expect("a section");
    assert!(s.contains(", indexed:"), "the file says it is indexed: {s}");
    assert!(s.contains("headings and line ranges only"), "{s}");
    // The deep body is absent — that is the whole point of the rule.
    assert!(!s.contains(&body), "the index carries no filler body");
    // The references a `read` needs: heading, span, first sentence. The title
    // spans lines 1-4 (its own line to the line before the next heading) and
    // Deep spans 5-7 to the end of the file.
    assert!(s.contains("1-4  # Title"), "{s}");
    assert!(s.contains("First sentence under the title."), "{s}");
    assert!(
        s.contains("5-7  ## Deep"),
        "the Deep heading's span is addressed: {s}"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

/// **The decision is per file.** A ten-line note does not pay for a
/// four-hundred-line one — whichever side of the order each sits on.
#[test]
fn a_small_note_is_whole_beside_a_large_one() {
    let body = over_budget();
    // The large file first: it is indexed, and the small note behind it still
    // arrives whole out of what is left of the budget.
    let ws = dir("mixed");
    write(&ws.join("AGENTS.md"), &format!("# Big\n\n{body}\n"));
    write(
        &ws.join(".letibot/notes/small.md"),
        "the small note's one line\n",
    );
    let s = section(&ws, &dir("none"), &vocab()).expect("a section");
    assert!(s.contains(", indexed:"), "the large file is indexed: {s}");
    assert!(
        s.contains("the small note's one line"),
        "the small note is whole beside it: {s}"
    );
    assert!(!s.contains(&body), "and the large file's body is not: {s}");

    // The other way round: a small `AGENTS.md` is whole, and the large project
    // note behind it is the one indexed.
    let ws2 = dir("mixed2");
    write(
        &ws2.join("AGENTS.md"),
        "# Rules\n\nShort and to the point.\n",
    );
    write(
        &ws2.join(".letibot/notes/big.md"),
        &format!("# Big\n\n{body}\n"),
    );
    let s2 = section(&ws2, &dir("none"), &vocab()).expect("a section");
    assert!(
        s2.contains("Short and to the point."),
        "AGENTS.md is whole: {s2}"
    );
    assert!(
        s2.contains("big.md") && s2.contains(", indexed:"),
        "the large note is indexed: {s2}"
    );
    assert!(!s2.contains(&body), "and its body is not injected: {s2}");
    let _ = std::fs::remove_dir_all(&ws);
    let _ = std::fs::remove_dir_all(&ws2);
}

/// **A headingless note over the budget is indexed by its paragraphs, not
/// collapsed to one line** — and its abstract is the first *proper* sentence.
///
/// The two defects this closes, both measured on the corpus this module reads:
/// a note with no headings arrived as one truncated line — nothing to recognise
/// and no range to `read` — and the "first sentence or two" rule counted every
/// period, so a lead ended inside `exit 101 in 2.` (the period of `2.7 s`).
#[test]
fn a_headingless_note_is_indexed_by_its_paragraphs() {
    let ws = dir("headingless");
    let filler: String = "and more filler that exists only to spend the budget. ".repeat(60);
    let note = format!(
        "The rano pin is absent on this box; the build exits 101 in 2.7 s.\n\
         Build from outside the repo.\n\n\
         A second paragraph says something else entirely. It has its own range.\n\n\
         {filler}\n"
    );
    write(&ws.join(".letibot/notes/pin.md"), &note);
    let s = section(&ws, &dir("none"), &vocab()).expect("a section");
    assert!(s.contains("pin.md") && s.contains(", indexed:"), "{s}");
    assert!(
        !s.contains("(no headings"),
        "the one-line collapse is gone: {s}"
    );
    assert!(
        s.contains(
            "abstract (derived): The rano pin is absent on this box; the build exits 101 in 2.7 s."
        ),
        "the abstract is the first proper sentence, not a cut inside `2.`: {s}"
    );
    // Every paragraph is addressed, with the file's own line numbers.
    let rows: Vec<&str> = s
        .lines()
        .filter(|l| l.trim_start().starts_with(|c: char| c.is_ascii_digit()))
        .collect();
    assert_eq!(rows.len(), 3, "three paragraphs, three ranges: {s}");
    assert!(
        s.contains("1-2  para — The rano pin is absent on this box; the build exits 101 in 2.7 s."),
        "{s}"
    );
    assert!(
        s.contains("4-4  para — A second paragraph says something else entirely."),
        "{s}"
    );
    assert!(
        s.contains("6-6  para"),
        "the last paragraph has its range: {s}"
    );
    assert!(!s.contains(&filler), "the body itself is not injected: {s}");
    let _ = std::fs::remove_dir_all(&ws);
}

/// **A period inside a filename is not a sentence end either** — the other
/// measured cut (`merge_review.attempts, .`), which is what a rule that counted
/// every period produced.
#[test]
fn the_abstract_never_ends_inside_a_path() {
    let ws = dir("path");
    write(
        &ws.join("AGENTS.md"),
        &format!(
            "See merge_review.attempts, .5 of the rows. Then read it.\n\n{}\n",
            over_budget()
        ),
    );
    let s = section(&ws, &dir("none"), &vocab()).expect("a section");
    assert!(
        s.contains("abstract (derived): See merge_review.attempts, .5 of the rows."),
        "the sentence ends at the sentence, not at the path: {s}"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

/// **The author's abstract is the index's first line when the file carries one,
/// and it is not called derived.**
///
/// The `notes` tool writes that marker line; this is the reader's half of the
/// round trip, and the reason the marker is an HTML comment — the same file is
/// injected verbatim under the budget, where a marker that rendered would be a
/// line the author did not write.
#[test]
fn an_authors_abstract_leads_the_index() {
    let ws = dir("written");
    write(
        &ws.join(".letibot/notes/pin.md"),
        &format!(
            "<!-- abstract: the rano pin is absent on this box -->\n\n\
             First prose line here. And more of it.\n\n{}\n",
            over_budget()
        ),
    );
    let s = section(&ws, &dir("none"), &vocab()).expect("a section");
    assert!(
        s.contains("  abstract: the rano pin is absent on this box"),
        "the author's own line leads: {s}"
    );
    assert!(
        !s.contains("abstract (derived)"),
        "and it is not attributed to the harness: {s}"
    );
    // The marker is a line of the file like any other: the paragraphs after it
    // are addressed by their real line numbers.
    assert!(s.contains("3-3  para — First prose line here."), "{s}");
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

/// **The order is the rule `gather` states**: `AGENTS.md`, then the box-wide
/// notes, then the project's — each directory newest first, ties by name.
#[test]
fn the_sources_keep_the_order_the_rule_states() {
    let ws = dir("order");
    let global = dir("global");
    write(&ws.join("AGENTS.md"), "agents\n");
    write(&global.join("a-old.md"), "global old\n");
    write(&global.join("b-new.md"), "global new\n");
    write(&ws.join(".letibot/notes/a-old.md"), "project old\n");
    write(&ws.join(".letibot/notes/b-new.md"), "project new\n");
    write(&ws.join(".letibot/notes/c-tie.md"), "project tie c\n");
    write(&ws.join(".letibot/notes/d-tie.md"), "project tie d\n");
    for (path, secs) in [
        (&global.join("a-old.md"), 1_000),
        (&global.join("b-new.md"), 2_000),
        (&ws.join(".letibot/notes/a-old.md"), 1_000),
        (&ws.join(".letibot/notes/b-new.md"), 2_000),
        (&ws.join(".letibot/notes/c-tie.md"), 1_000),
        (&ws.join(".letibot/notes/d-tie.md"), 1_000),
    ] {
        set_mtime(path, secs);
    }
    let s = section(&ws, &global, &vocab()).expect("a section");
    let pos = |needle: &str| s.find(needle).expect(needle);
    assert!(
        pos("agents\n") < pos("global new\n")
            && pos("global new\n") < pos("global old\n")
            && pos("global old\n") < pos("project new\n")
            && pos("project new\n") < pos("project old\n"),
        "AGENTS.md, then box-wide newest first, then the project's newest first: {s}"
    );
    assert!(
        pos("project tie c\n") < pos("project tie d\n"),
        "an mtime tie is broken by name, so the order is a function of the files: {s}"
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

/// **The budget's edge: at it verbatim, one token over it an index.**
#[test]
fn the_budget_edge_is_exact() {
    let ws = dir("edge");
    let path = ws.join("AGENTS.md");
    // One byte = one token under the byte vocabulary, and the budget is decided
    // on the assembled BODY (the path heading plus the files) — the envelope is
    // the harness's own fixed sentence, not the operator's material, and
    // budgeting it would spend the operator's allowance on our wording. Build the
    // file so the body sits exactly on the budget.
    let overhead = format!("### {}\n", path.display()).len();
    let file = "a".repeat(NOTES_BUDGET_TOKENS - overhead);
    write(&path, &file);
    let s = section(&ws, &dir("none"), &vocab()).unwrap();
    assert!(s.contains(&file), "exactly at the budget: verbatim");
    write(&path, &format!("{file}a"));
    let over = section(&ws, &dir("none"), &vocab()).unwrap();
    assert!(over.contains(", indexed:"), "one token over: {over}");
    assert!(
        !over.contains(&format!("{file}a")),
        "and the text is not carried: {over}"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

/// **`replace` swaps the section and leaves the rest byte for byte.**
#[test]
fn replace_swaps_only_the_section() {
    let ws = dir("replace");
    write(&ws.join("AGENTS.md"), "first\n");
    let one = section(&ws, &dir("none"), &vocab()).unwrap();
    // A composed prompt as `with_fabric` leaves it: sections, notes, then a
    // fabric block appended after.
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
    // Removing: no doubled blank line where the section was, mid-prompt or at
    // the end — a removal from the end takes its glue with it.
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

/// **The envelope says whose notes these are, that they are historical, and
/// what an indexed file is.**
///
/// The operator's ruling, 2026-10-09: *"The read tool should carry a short note
/// - 'it is a historical note, might be outdated'"* — and the injected block is
/// the highest-traffic read of all, delivered at session open and every base
/// rebuild whether or not the tool is ever called. The envelope also had to
/// change for a second reason: the project's `.letibot/notes/` is now writable
/// by sessions through the `notes` tool, so a blanket "the operator maintains
/// these" stopped being true. The authorship split is by directory — the tell
/// that keeps a note the model wrote from reading as an instruction the operator
/// gave — and the caveat says what a note is: a record of what was true when
/// written, not a fact about now. The last sentence is the third reason: the
/// block is now verbatim *and* indexed at once, and a reader who does not know
/// what `indexed` means will read a heading as the file's whole content.
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
    assert!(
        s.contains("did not fit the budget"),
        "and the mixed block says what an indexed heading is: {s}"
    );
    let _ = std::fs::remove_dir_all(&ws);
}
