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
    let wrote =
        std::fs::read_to_string(ws.join(".letibot/notes/finding.md")).expect("the note is on disk");
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

/// **The two other doors into the notes directory are refused, and the tool
/// that owns it still writes.**
///
/// The hole this closes was named in this module's own doc: a note put in
/// `.letibot/notes/` by `write` or `edit` never reaches [`DUPLICATE_FLOOR`]'s
/// gate, and the reader injects it exactly as it injects the notes that were
/// scored. The second half is the half that must not break — the fix is a rule
/// about two OTHER doors, and a rule that also caught this tool's own writes
/// would be worse than the hole.
///
/// The spellings are the ones a caller actually produces: the plain one, the
/// `./`-prefixed one, one that climbs out and comes back, and the absolute path
/// of the same file. A rule that read the spelling rather than the fact would
/// be this same defect one door over, so the whole list is one case.
#[test]
fn the_other_doors_into_the_notes_dir_are_refused_and_the_tool_still_writes() {
    let mut h = writable_harness();
    let (ws, _) = h.notes_dirs();
    // The absolute spelling is compared against the backend's own cwd, which is
    // canonicalised when the backend opens — so the fixture's path is too.
    let abs = std::fs::canonicalize(&ws).unwrap_or_else(|_| ws.clone());
    std::fs::create_dir_all(ws.join(NOTES_DIR)).expect("project dir");

    for path in [
        format!("{NOTES_DIR}/by-hand.md"),
        format!("./{NOTES_DIR}/by-hand.md"),
        format!("src/../{NOTES_DIR}/by-hand.md"),
        abs.join(NOTES_DIR).join("by-hand.md").display().to_string(),
    ] {
        let args = format!(r#"{{"path":{path:?},"content":"a note nobody scored\n"}}"#);
        let r = h.call("write", &args);
        assert!(!r.is_grounded(), "`{path}` must be refused: {}", r.render());
        let said = r.render();
        assert!(
            said.contains("which the `notes` tool owns"),
            "the refusal is by name — the directory and the tool that owns it: {said}"
        );
        assert!(said.contains("Nothing was written"), "{said}");
        assert!(
            said.contains("near-copy") && said.contains("\"append\""),
            "and it says what to do instead, in the verbs that do it: {said}"
        );
    }

    // **`edit`, in both of its shapes.** The check is before the `edits`
    // dispatch, so a batch is not a way round it.
    std::fs::write(ws.join(NOTES_DIR).join("existing.md"), "a note\n").expect("fixture");
    for args in [
        format!(
            r#"{{"path":"{NOTES_DIR}/existing.md","old_string":"a note","new_string":"b note"}}"#
        ),
        format!(
            r#"{{"path":"{NOTES_DIR}/existing.md","edits":[{{"old_string":"a note","new_string":"b note"}}]}}"#
        ),
    ] {
        let r = h.call("edit", &args);
        assert!(!r.is_grounded(), "{args}: {}", r.render());
        assert!(
            r.render().contains("which the `notes` tool owns"),
            "{args}: {}",
            r.render()
        );
    }
    assert_eq!(
        std::fs::read_to_string(ws.join(NOTES_DIR).join("existing.md")).unwrap(),
        "a note\n",
        "no spelling of a write or an edit reached the note"
    );
    assert!(
        !ws.join(NOTES_DIR).join("by-hand.md").exists(),
        "and none of the four `write` spellings landed a file"
    );

    // **And the door that owns the directory still opens.** A rule that caught
    // the wrong door would show up here and nowhere else.
    let r = h.call(
        "notes",
        r#"{"action":"add","name":"scored","text":"the gate scored this one before it landed"}"#,
    );
    assert!(
        r.is_grounded(),
        "the notes tool still writes: {}",
        r.render()
    );
    assert_eq!(
        std::fs::read_to_string(ws.join(NOTES_DIR).join("scored.md")).unwrap(),
        "the gate scored this one before it landed\n"
    );
    // And the gate on that door is still the gate — a verbatim copy under a new
    // name is the shape the floor was measured against.
    let r = h.call(
        "notes",
        r#"{"action":"add","name":"scored-again","text":"the gate scored this one before it landed"}"#,
    );
    assert!(
        !r.is_grounded(),
        "the similarity gate still bites, on the tool's own door: {}",
        r.render()
    );
    assert!(r.render().contains("near-copy"), "{}", r.render());
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
