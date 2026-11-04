//! The census: this module's rules, asserted.

use super::*;

/// The byte vocabulary: one token per byte, no GGUF, no FFI, so the budget arithmetic here is exact
/// rather than proportional.
fn vocab() -> Vocab {
    Vocab::bytes([], [])
}

/// A scratch dir of this test's own, one per CALL rather than per name: twenty tests ask for
/// `dir("none")` and would otherwise share one path and race each other's `remove_dir_all`.
fn dir(name: &str) -> PathBuf {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let d = letibot_sessionlog::testing::scratch_root().join(format!(
        "letibot-standing-{}-{}-{name}",
        std::process::id(),
        N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("temp dir");
    // The reader spells a workspace as its backend does, which is the resolved path: a fixture
    // that asserted a heading by the name it typed would otherwise be testing `/tmp` on a Mac.
    d.canonicalize().expect("a real path")
}

/// The section of a tree on THIS box: every fixture here is local, so the reader is the
/// production one over a host backend rooted where the fixture wrote its files.
fn with_reader<T>(ws: &Path, f: impl FnOnce(&Reader<'_>) -> T) -> T {
    let backend = letibot_tools::backend::HostBackend::new(ws).expect("a fixture root");
    f(&Reader::new(&backend, ws))
}

fn section(
    ws: &Path,
    global: &Path,
    box_dirs: &crate::leticode_config::BoxDirs,
    vocab: &Vocab,
) -> Option<String> {
    with_reader(ws, |files| super::section(files, global, box_dirs, vocab))
}

fn section_with_skills(
    ws: &Path,
    global: &Path,
    box_dirs: &crate::leticode_config::BoxDirs,
    vocab: &Vocab,
    skills: Option<&SkillRegistry>,
    preloaded: Option<&str>,
) -> Option<String> {
    with_reader(ws, |files| {
        super::section_with_skills(files, global, box_dirs, vocab, skills, preloaded)
    })
}

fn rows(
    ws: &Path,
    global: &Path,
    box_dirs: &crate::leticode_config::BoxDirs,
    vocab: &Vocab,
) -> Vec<NoteEntry> {
    with_reader(ws, |files| super::rows(files, global, box_dirs, vocab))
}

fn write(path: &Path, text: &str) {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).expect("parent");
    }
    std::fs::write(path, text).expect("fixture");
}

/// A box with no instruction file of its own: every fixture points the box scope at scratch dirs
/// that hold nothing, so the box the suite RUNS on cannot answer for it.
fn no_box() -> crate::leticode_config::BoxDirs {
    crate::leticode_config::BoxDirs {
        config: dir("box-config"),
        home: dir("box-home"),
    }
}

/// A file's mtime, stated rather than waited for: a sleep between two writes would make the
/// assertion depend on the clock's resolution instead of on the rule.
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

/// A note whose body alone is over the budget, for the tests that need one file indexed and care
/// about nothing else in it.
fn over_budget() -> String {
    "filler sentence that exists only to spend the budget. ".repeat(80)
}

/// Under the budget the files are whole, each under its path.
#[test]
fn under_the_budget_the_files_are_injected_verbatim() {
    let ws = dir("verbatim");
    write(
        &ws.join("AGENTS.md"),
        "# Rules\n\nBuild with `cargo test -p`, never `--workspace`.\n",
    );
    let s = section(&ws, &dir("none"), &no_box(), &vocab()).expect("a section");
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

/// Over the budget a file is an index with line references rather than its text, and its heading
/// says so.
#[test]
fn over_the_budget_it_is_an_index_with_line_references() {
    let ws = dir("index");
    let body = over_budget();
    let file = format!(
        "# Title\n\nFirst sentence under the title. Second sentence, rarely needed.\n\n## Deep\n\n{body}\n"
    );
    write(&ws.join("AGENTS.md"), &file);
    assert!(file.len() > NOTES_BUDGET_TOKENS, "the fixture must exceed");
    let s = section(&ws, &dir("none"), &no_box(), &vocab()).expect("a section");
    assert!(s.contains(", indexed:"), "the file says it is indexed: {s}");
    assert!(s.contains("headings and line ranges only"), "{s}");
    // The deep body is absent - that is the whole point of the rule.
    assert!(!s.contains(&body), "the index carries no filler body");
    // The references a `read` needs: heading, span, first sentence. The title spans lines 1-4 and
    // Deep runs from 5 to the end of the file.
    assert!(s.contains("1-4  # Title"), "{s}");
    assert!(s.contains("First sentence under the title."), "{s}");
    assert!(
        s.contains("5-7  ## Deep"),
        "the Deep heading's span is addressed: {s}"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

/// The decision is per file: a ten-line note does not pay for a four-hundred-line one, whichever
/// side of the order each sits on.
#[test]
fn a_small_note_is_whole_beside_a_large_one() {
    let body = over_budget();
    // The large file first: it is indexed, and the small note behind it still arrives whole out of
    // what is left of the budget.
    let ws = dir("mixed");
    write(&ws.join("AGENTS.md"), &format!("# Big\n\n{body}\n"));
    write(
        &ws.join(".letibot/notes/small.md"),
        "the small note's one line\n",
    );
    let s = section(&ws, &dir("none"), &no_box(), &vocab()).expect("a section");
    assert!(s.contains(", indexed:"), "the large file is indexed: {s}");
    assert!(
        s.contains("the small note's one line"),
        "the small note is whole beside it: {s}"
    );
    assert!(!s.contains(&body), "and the large file's body is not: {s}");

    // The other way round: a small `AGENTS.md` is whole, and the large project note behind it is
    // the one indexed.
    let ws2 = dir("mixed2");
    write(
        &ws2.join("AGENTS.md"),
        "# Rules\n\nShort and to the point.\n",
    );
    write(
        &ws2.join(".letibot/notes/big.md"),
        &format!("# Big\n\n{body}\n"),
    );
    let s2 = section(&ws2, &dir("none"), &no_box(), &vocab()).expect("a section");
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

/// A headingless note over the budget is indexed by its paragraphs rather than collapsed to
/// one line, and its abstract is the first proper sentence.
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
    let s = section(&ws, &dir("none"), &no_box(), &vocab()).expect("a section");
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

/// A period inside a filename is not a sentence end either: a rule that counted every period ends
/// the lead at `merge_review.attempts,.`.
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
    let s = section(&ws, &dir("none"), &no_box(), &vocab()).expect("a section");
    assert!(
        s.contains("abstract (derived): See merge_review.attempts, .5 of the rows."),
        "the sentence ends at the sentence, not at the path: {s}"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

/// The author's abstract is the index's first line when the file carries one, and it is not
/// called derived. The `notes` tool writes that marker line, so this is the reader's half
/// of the round trip.
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
    let s = section(&ws, &dir("none"), &no_box(), &vocab()).expect("a section");
    assert!(
        s.contains("  abstract: the rano pin is absent on this box"),
        "the author's own line leads: {s}"
    );
    assert!(
        !s.contains("abstract (derived)"),
        "and it is not attributed to the harness: {s}"
    );
    // The marker is a line of the file like any other: the paragraphs after it are addressed by
    // their real line numbers.
    assert!(s.contains("3-3  para — First prose line here."), "{s}");
    let _ = std::fs::remove_dir_all(&ws);
}

/// A source that is absent contributes nothing: never a heading, never an error.
#[test]
fn a_missing_source_contributes_nothing() {
    let empty = dir("empty");
    assert!(
        section(&empty, &dir("none"), &no_box(), &vocab()).is_none(),
        "no sources is no section"
    );
    let ws = dir("one");
    write(&ws.join("AGENTS.md"), "only file\n");
    let s = section(&ws, &dir("none"), &no_box(), &vocab()).expect("a section");
    assert_eq!(s.matches("### ").count(), 1, "one file, one heading: {s}");
    assert!(
        !s.contains("notes]"),
        "no empty heading for the absent dirs"
    );
    let _ = std::fs::remove_dir_all(&ws);
    let _ = std::fs::remove_dir_all(&empty);
}

/// A lowercase `agents.md` is the project's instructions too: Linux hosts are case-sensitive.
#[test]
fn a_lowercase_agents_md_is_read() {
    let ws = dir("lower");
    write(&ws.join("agents.md"), "lowercase rules\n");
    let s = section(&ws, &dir("none"), &no_box(), &vocab()).expect("a section");
    assert!(s.contains("lowercase rules"), "{s}");
    let _ = std::fs::remove_dir_all(&ws);
}

/// The order is the one `gather` states: `AGENTS.md`, then the box-wide notes, then the project's
/// each directory newest first and ties by name.
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
    let s = section(&ws, &global, &no_box(), &vocab()).expect("a section");
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

/// The same files assemble to the same section, twice.
#[test]
fn the_same_files_assemble_to_the_same_section() {
    let ws = dir("same");
    write(&ws.join("AGENTS.md"), "# One\n\ntext\n");
    let a = section(&ws, &dir("none"), &no_box(), &vocab());
    let b = section(&ws, &dir("none"), &no_box(), &vocab());
    assert_eq!(a, b);
    let _ = std::fs::remove_dir_all(&ws);
}

/// The budget's edge: at it verbatim, one token over it an index.
#[test]
fn the_budget_edge_is_exact() {
    let ws = dir("edge");
    let path = ws.join("AGENTS.md");
    // One byte is one token under the byte vocabulary, and the budget is decided on the assembled
    // BODY: the envelope is the harness's own fixed sentence, and budgeting it would spend the
    // operator's allowance on our wording.
    let overhead = format!("### {}\n", path.display()).len();
    let file = "a".repeat(NOTES_BUDGET_TOKENS - overhead);
    write(&path, &file);
    let s = section(&ws, &dir("none"), &no_box(), &vocab()).unwrap();
    assert!(s.contains(&file), "exactly at the budget: verbatim");
    write(&path, &format!("{file}a"));
    let over = section(&ws, &dir("none"), &no_box(), &vocab()).unwrap();
    assert!(over.contains(", indexed:"), "one token over: {over}");
    assert!(
        !over.contains(&format!("{file}a")),
        "and the text is not carried: {over}"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

/// `replace` swaps the section and leaves the rest byte for byte.
#[test]
fn replace_swaps_only_the_section() {
    let ws = dir("replace");
    write(&ws.join("AGENTS.md"), "first\n");
    let one = section(&ws, &dir("none"), &no_box(), &vocab()).unwrap();
    // A composed prompt as `with_fabric` leaves it: sections, notes, then a fabric block appended
    // after.
    let composed = format!("sections\n\n{one}\n\nfabric block");
    write(&ws.join("AGENTS.md"), "second\n");
    let two = section(&ws, &dir("none"), &no_box(), &vocab()).unwrap();
    let swapped = replace(&composed, Some(&two));
    assert!(swapped.contains("second"), "{swapped}");
    assert!(!swapped.contains("first"), "{swapped}");
    assert!(
        swapped.starts_with("sections\n") && swapped.ends_with("fabric block"),
        "everything outside the markers is carried through: {swapped}"
    );
    assert_eq!(carried(&swapped), Some(two.as_str()));
    // Removing: no doubled blank line where the section was, mid-prompt or at the end. A removal
    // from the end takes its glue with it.
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

/// `carried` reads the section back off a composed prompt.
#[test]
fn carried_reads_the_section_off_a_composed_prompt() {
    let ws = dir("carried");
    write(&ws.join("AGENTS.md"), "text\n");
    let s = section(&ws, &dir("none"), &no_box(), &vocab()).unwrap();
    assert_eq!(carried(&format!("before\n\n{s}\nafter")), Some(s.as_str()));
    assert_eq!(carried("no section here"), None);
    let _ = std::fs::remove_dir_all(&ws);
}

/// The envelope says whose notes these are, that they are historical, and what an indexed
/// file is. The block is the highest-traffic read of all, delivered at session open and
/// every base rebuild whether or not a tool is called.
#[test]
fn the_envelope_splits_authorship_and_carries_the_historical_caveat() {
    let ws = dir("envelope");
    write(
        &ws.join(".letibot/notes/session-find.md"),
        "a session wrote this\n",
    );
    let s = section(&ws, &dir("none"), &no_box(), &vocab()).unwrap();
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

/// The pane's rows: one per note, in the section's order, carrying the index's own fields.
/// This is the one consumer a PERSON reads, so what it asserts is what the pane would draw
/// and nothing more
#[test]
fn the_panes_rows_are_the_indexes_own_fields() {
    let ws = dir("rows");
    write(
        &ws.join("AGENTS.md"),
        "# Rules\n\nBuild with `cargo test -p`, never `--workspace`.\n",
    );
    let body = over_budget();
    write(
        &ws.join(".letibot/notes/big.md"),
        &format!("# Big\n\nFirst sentence of the big note. More after it.\n\n{body}\n"),
    );
    write(
        &ws.join(".letibot/notes/small.md"),
        "<!-- abstract: the author's own line -->\n\nthe small note's prose\n",
    );
    let rows = rows(&ws, &dir("none"), &no_box(), &vocab());
    assert_eq!(
        rows.len(),
        3,
        "one row per note the harness reads: {rows:?}"
    );
    // The section's order: AGENTS.md first, then the project's, newest first.
    assert!(rows[0].path.ends_with("AGENTS.md"), "{rows:?}");
    assert_eq!(rows[0].form, NoteForm::Verbatim);
    assert_eq!(
        rows[0].abstract_line.as_deref(),
        Some("Build with `cargo test -p`, never `--workspace`."),
        "the first proper sentence, as the index has it"
    );
    assert!(!rows[0].abstract_written, "derived, and said to be derived");

    let big = rows
        .iter()
        .find(|r| r.path.ends_with("big.md"))
        .expect("a row for the big note");
    assert_eq!(
        big.form,
        NoteForm::Indexed,
        "over the budget, so the prompt carries its index"
    );
    assert_eq!(
        big.abstract_line.as_deref(),
        Some("First sentence of the big note."),
        "the abstract is the index's own line for a note that IS indexed"
    );

    let small = rows
        .iter()
        .find(|r| r.path.ends_with("small.md"))
        .expect("a row for the small note");
    assert_eq!(
        small.form,
        NoteForm::Verbatim,
        "the small note beside the large one is still whole"
    );
    assert_eq!(
        small.abstract_line.as_deref(),
        Some("the author's own line"),
        "the author's marker, not the derived sentence"
    );
    assert!(
        small.abstract_written,
        "and the row says it is the author's rather than the harness's reading"
    );

    // And the rows and the section are ONE walk: the form each file was given above is the form the
    // prompt carries it in, which is why `plan` exists.
    let s = section(&ws, &dir("none"), &no_box(), &vocab()).expect("a section");
    assert!(s.contains("First sentence of the big note."), "{s}");
    assert!(!s.contains(&body), "the big note is indexed, not injected");
    assert!(
        s.contains("the small note's prose"),
        "and the small one is whole: {s}"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

/// An empty corpus is no rows rather than a row that says nothing. The pane says so itself
/// (`ui/panes/standing.rs`); what this pins is that the daemon hands it nothing to draw.
#[test]
fn no_corpus_is_no_rows() {
    let ws = dir("rows-empty");
    assert!(rows(&ws, &dir("none"), &no_box(), &vocab()).is_empty());
    let _ = std::fs::remove_dir_all(&ws);
}

/// The seat's instruction file is a source, and it sits with `AGENTS.md` rather than behind the
/// notes, because a rule that arrives as an index is a rule the model has to fetch first.
#[test]
fn the_seats_instruction_file_is_read_first() {
    let ws = dir("seat-file");
    let global = dir("seat-file-global");
    write(&ws.join("AGENTS.md"), "the project's own rules\n");
    let seat = ws.join(crate::leticode_config::INSTRUCTION_FILE);
    write(&seat, "how a leticode works here\n");
    write(&global.join("box-note.md"), "a box-wide note\n");
    let s = section(&ws, &global, &no_box(), &vocab()).expect("a section");
    let pos = |needle: &str| s.find(needle).expect(needle);
    assert!(
        s.contains(&format!("### {}", seat.display())),
        "the path is the heading, as it is for every other source: {s}"
    );
    assert!(s.contains("how a leticode works here"), "{s}");
    assert!(
        pos("the project's own rules") < pos("how a leticode works here")
            && pos("how a leticode works here") < pos("a box-wide note"),
        "AGENTS.md, then the seat's file, then the notes: {s}"
    );
    let _ = std::fs::remove_dir_all(&ws);
    let _ = std::fs::remove_dir_all(&global);
}

/// The canonical name answers when the seat's own is not there, so a project written for another
/// tool still reaches this one.
#[test]
fn the_canonical_instruction_file_answers_when_the_seat_has_none() {
    let ws = dir("canonical-file");
    let file = ws.join(crate::leticode_config::CANONICAL_INSTRUCTION_FILE);
    write(&file, "written for another tool\n");
    let s = section(&ws, &dir("none"), &no_box(), &vocab()).expect("a section");
    assert!(s.contains(&format!("### {}", file.display())), "{s}");
    assert!(s.contains("written for another tool"), "{s}");
    let _ = std::fs::remove_dir_all(&ws);
}

/// A file above the workspace is found, so a session started in a subdirectory reads the project's
/// instructions: the walk-up a `.env` has.
#[test]
fn an_instruction_file_above_the_workspace_is_found() {
    let root = dir("above");
    write(
        &root.join(crate::leticode_config::INSTRUCTION_FILE),
        "the project root's words\n",
    );
    let deep = root.join("crates").join("harnessd");
    std::fs::create_dir_all(&deep).expect("the workspace");
    let s = section(&deep, &dir("none"), &no_box(), &vocab()).expect("a section");
    assert!(s.contains("the project root's words"), "{s}");
    let _ = std::fs::remove_dir_all(&root);
}

/// The box's file is read too, after the workspace's, from the config dir the seat already uses.
/// The preference between the two names is the config layer's own test.
#[test]
fn the_boxs_instruction_file_is_read_after_the_workspaces() {
    let ws = dir("box-file-ws");
    write(&ws.join("AGENTS.md"), "the project's own rules\n");
    write(
        &ws.join(crate::leticode_config::INSTRUCTION_FILE),
        "how a leticode works in this project\n",
    );
    let box_dirs = no_box();
    write(
        &box_dirs
            .config
            .join(crate::leticode_config::INSTRUCTION_FILE),
        "how a leticode works on this box\n",
    );
    let s = section(&ws, &dir("none"), &box_dirs, &vocab()).expect("a section");
    let pos = |needle: &str| s.find(needle).expect(needle);
    assert!(s.contains("how a leticode works on this box"), "{s}");
    assert!(
        pos("the project's own rules") < pos("how a leticode works in this project")
            && pos("how a leticode works in this project")
                < pos("how a leticode works on this box"),
        "the more specific statement comes first: {s}"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

/// A box with neither name adds nothing, not a heading and not an error, which is what makes asking
/// on every session open safe.
#[test]
fn a_box_with_no_instruction_file_adds_nothing() {
    let ws = dir("box-none");
    write(&ws.join("AGENTS.md"), "only file\n");
    let s = section(&ws, &dir("none"), &no_box(), &vocab()).expect("a section");
    assert_eq!(s.matches("### ").count(), 1, "one file, one heading: {s}");
    let _ = std::fs::remove_dir_all(&ws);
}

/// The names are the seat's, and this crate holds no copy of either. No file under
/// `harnessd`, `tokencore` or `tools` spells either name, so a runtime that grew one back
/// fails here rather than in a review.
#[test]
fn the_runtime_names_neither_instruction_file() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the crates directory")
        .to_path_buf();
    let names = [
        crate::leticode_config::INSTRUCTION_FILE,
        crate::leticode_config::CANONICAL_INSTRUCTION_FILE,
    ];
    let me = Path::new(file!());
    let mut hits: Vec<String> = Vec::new();
    for krate in ["harnessd", "tokencore", "tools"] {
        let mut stack = vec![crates.join(krate)];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.ends_with(me) {
                    continue;
                }
                let Ok(bytes) = std::fs::read(&path) else {
                    continue;
                };
                for name in names {
                    if bytes.windows(name.len()).any(|w| w == name.as_bytes()) {
                        hits.push(format!("{}: {name}", path.display()));
                    }
                }
            }
        }
    }
    assert!(
        hits.is_empty(),
        "these names belong to the seat's config layer and to no file in the runtime: {hits:?}"
    );
}

/// The message that hands a skill over says whose words those are, and the rest of the line follows
/// it as the person's own prompt in the same turn.
#[test]
fn a_skill_handed_over_is_labelled_and_the_rest_of_the_line_follows_it() {
    let skill = letibot_tools::builtins::skill::Skill::new(
        "release",
        "cut a release",
        "tag, push, write the notes",
        letibot_tools::builtins::skill::SkillSource::Project,
    );
    let said = crate::harness::skill_message(&skill, "cut 0.5.2 please");
    assert!(
        said.starts_with("[skill `release`, from this project"),
        "{said}"
    );
    assert!(said.contains("the words below are the skill's"), "{said}");
    assert!(said.contains("tag, push, write the notes"), "{said}");
    assert!(said.ends_with("cut 0.5.2 please"), "{said}");
    // A bare `/skills release` is the body and nothing else.
    assert!(
        crate::harness::skill_message(&skill, "  ").ends_with("[end of the skill `release`]"),
        "no trailing prose"
    );
}

/// A skill a session was STARTED with arrives whole, and the name it was started with is
/// the one that is read. `preloaded` is `task {skills}`'s and `[queue] reviewer_skills`'s
/// landing place, so a name that resolves to nothing is said rather than dropped.
#[test]
fn a_declared_skill_arrives_whole_and_an_unknown_one_is_said() {
    let r = letibot_tools::builtins::skill::SkillRegistry::new(vec![
        letibot_tools::builtins::skill::Skill::new(
            "review-a-branch",
            "judge a branch",
            "read the diff, then run the tests",
            letibot_tools::builtins::skill::SkillSource::Project,
        ),
    ]);
    let block =
        preloaded(&r, &["review-a-branch".to_string(), "nope".to_string()]).expect("a block");
    assert!(
        block.contains("read the diff, then run the tests"),
        "{block}"
    );
    assert!(block.contains("[this project]"), "{block}");
    assert!(block.contains("nope — could not be read"), "{block}");
    // Nothing declared is nothing at all, not an empty heading in the prompt.
    assert!(preloaded(&r, &[]).is_none());
}

/// The skills index rides the same section, and no body comes with it: the pinned names and their
/// sources reach the prompt, the rest are a count, and the text arrives only when asked.
#[test]
fn the_section_carries_the_skills_index_and_never_a_body() {
    let ws = dir("skills");
    let g = dir("skills-global");
    let r = letibot_tools::builtins::skill::SkillRegistry::new(vec![
        letibot_tools::builtins::skill::Skill::new(
            "release",
            "cut a release",
            "THE BODY NOBODY IS GIVEN UNASKED",
            letibot_tools::builtins::skill::SkillSource::Project,
        ),
        letibot_tools::builtins::skill::Skill::new(
            "review",
            "judge a branch",
            "ANOTHER BODY NOBODY IS GIVEN",
            letibot_tools::builtins::skill::SkillSource::Installed(None),
        ),
    ])
    .with_pins(["release"]);
    let s = section_with_skills(&ws, &g, &no_box(), &vocab(), Some(&r), None).expect("a section");
    assert!(
        s.contains("- release  [this project]  cut a release"),
        "{s}"
    );
    assert!(!s.contains("THE BODY NOBODY IS GIVEN UNASKED"), "{s}");
    // The unpinned skill is a count and never a row: an index of every source spends the prompt's
    // first screen on names nobody asked to pin.
    assert!(
        !s.contains("- review"),
        "an unpinned skill is not listed: {s}"
    );
    assert!(s.contains("1 more that `skill list` holds"), "{s}");
    assert!(s.contains("1 pinned of 2"), "{s}");
    assert!(s.contains("`skill list` every one"), "{s}");
    // And the section says what the labels mean, so a model cannot take a model's skill for
    // the operator's instruction.
    assert!(s.contains("only a skill labelled"), "{s}");
    // The pane and the prompt are the same rule: a box with neither notes nor skills has no
    // section at all rather than an empty one.
    let empty = letibot_tools::builtins::skill::SkillRegistry::new(Vec::new());
    assert!(section_with_skills(&ws, &g, &no_box(), &vocab(), Some(&empty), None).is_none());
    // A skill this session was STARTED with arrives whole, while everything else stays an index of
    // names.
    let launched = section_with_skills(
        &ws,
        &g,
        &no_box(),
        &vocab(),
        Some(&r),
        Some("### the skills this session was started with\n#### release  [this project]\ncut it, then tell people\n"),
    )
    .expect("a section");
    assert!(launched.contains("cut it, then tell people"), "{launched}");
    assert!(launched.contains("- release  [this project]"), "{launched}");
    let _ = std::fs::remove_dir_all(&ws);
    let _ = std::fs::remove_dir_all(&g);
}

/// A note on the session's own backend is read into the prompt. A placed session's notes are
/// on that host, and the reader went to the daemon's path for the workspace instead: the note the
/// model had just written reached no prompt until the branch came home.
#[test]
fn a_note_on_the_session_backend_reaches_the_section() {
    let host = dir("placed-host");
    let scope = dir("placed-scope");
    write(
        &host.join(".letibot/notes/placed.md"),
        "the host's own note\n",
    );
    let s = super::section(
        &Reader::new(
            &letibot_tools::backend::HostBackend::writable(&host).expect("the session's root"),
            &scope,
        ),
        &dir("none"),
        &no_box(),
        &vocab(),
    );
    assert!(
        s.as_deref()
            .is_some_and(|s| s.contains("the host's own note")),
        "the note the session wrote on its own backend reached no prompt: {s:?}"
    );
    let _ = std::fs::remove_dir_all(&host);
    let _ = std::fs::remove_dir_all(&scope);
}
