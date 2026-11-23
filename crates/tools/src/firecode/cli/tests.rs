//! A stub `firecode` on disk, answering the documents the brief prints. A test names it by path,
//! so no test in this binary has to set `$FIRECODE_BIN` under every other reader of it.

use super::*;

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("letibot-fccli-{}-{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// `<root>/firecode`, a shell script whose `case "$1 $2"` arms are `arms`. It records every call's
/// argv in `<root>/calls`, one line per call, which is how a test asserts what was passed.
fn stub(root: &Path, arms: &str) -> PathBuf {
    let bin = root.join("firecode");
    crate::testing::write_stub(
        &bin,
        &format!(
            "#!/bin/sh\nROOT='{}'\n[ \"$#\" -eq 0 ] && exit 0\n\
             printf '%s\\n' \"$*\" >> \"$ROOT/calls\"\ncase \"$1 $2\" in\n{arms}\nesac\nexit 0\n",
            root.display()
        ),
    );
    bin
}

fn said(root: &Path) -> Vec<String> {
    std::fs::read_to_string(root.join("calls"))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

/// `extraction: out` is the only spelling of "the work came out", and exit 3 is a document, not a
/// failed call: it is the refusal that leaves a copy to keep.
#[test]
fn a_release_says_whether_the_guest_work_came_out() {
    let root = tmp("release-out");
    let bin = stub(
        &root,
        "\"state release\") echo '{\"released\":true,\"name\":\"proj-ab12\",\"purpose\":\"reviewer\",\
         \"extraction\":\"out\",\"bytes\":131072}' ;;\n\
         * ) exit 0 ;;",
    );
    let r = release(&bin, Which::Name("proj-ab12")).expect("a document");
    assert!(r.released && r.work_is_out());
    assert_eq!(r.bytes, Some(131072));
    assert!(r.sentence("proj-ab12").contains("released, work out"));

    // By project, as a backend that never learned the name asks.
    let r = release(&bin, Which::Project(Path::new("/cache/child"))).expect("a document");
    assert!(r.work_is_out() || !r.released);
    let calls = said(&root);
    assert_eq!(calls[0], "state release proj-ab12 --json");
    assert_eq!(calls[1], "state release --project /cache/child --json");
}

/// Exit 3, `unextracted: true`: the caller must NOT delete the copy, and the line it tells a
/// person carries firecode's own hint.
#[test]
fn a_refused_release_is_read_not_treated_as_a_failure() {
    let root = tmp("release-refused");
    let bin = stub(
        &root,
        "\"state release\") echo '{\"released\":false,\"name\":\"proj-ab12\",\"reason\":\"its work \
         was never copied out\",\"unextracted\":true,\"hint\":\"firecode extract r1 --bundle b\"}' \
         ; exit 3 ;;\n* ) exit 0 ;;",
    );
    let r = release(&bin, Which::Name("proj-ab12")).expect("exit 3 is an answer");
    assert!(!r.released);
    assert!(!r.work_is_out(), "a refusal is not work that came out");
    let line = r.sentence("proj-ab12");
    assert!(line.contains("never copied out"), "{line}");
    assert!(line.contains("firecode extract r1 --bundle b"), "{line}");
}

/// `taken: false` is the answer a reviewer boots fresh on; a take names the project it hands over.
#[test]
fn a_pool_take_answers_empty_or_a_vm() {
    let root = tmp("take");
    let bin = stub(
        &root,
        "\"pool take\") echo '{\"taken\":false,\"reason\":\"empty\",\"project\":\"/w\"}' ;;\n* ) exit 0 ;;",
    );
    let t = take(&bin, Path::new("/w"), Purpose::Reviewer).expect("a document");
    assert!(!t.taken);
    assert_eq!(t.reason.as_deref(), Some("empty"));
    let call = &said(&root)[0];
    assert_eq!(call, "pool take --project /w --purpose reviewer --json");

    let root2 = tmp("take-one");
    let bin = stub(
        &root2,
        "\"pool take\") echo '{\"taken\":true,\"name\":\"proj-ab12cd34\",\"state\":\"in-use\",\
         \"was\":\"sleeping\",\"resume\":true,\"project_path\":\"/x/proj-ab12cd34\"}' ;;\n\
         * ) exit 0 ;;",
    );
    let t = take(&bin, Path::new("/w"), Purpose::Reviewer).expect("a document");
    assert!(t.taken && t.resume);
    assert_eq!(t.name.as_deref(), Some("proj-ab12cd34"));
    assert_eq!(
        t.project_path.as_deref(),
        Some(Path::new("/x/proj-ab12cd34"))
    );
}

/// Exit 1 with `running_processes` is firecode saying the guest still works: it is an answer, and
/// the one fact that decides whether `--kill` is asked for.
#[test]
fn a_return_whose_guest_still_runs_is_read_and_says_so() {
    let root = tmp("return");
    let bin = stub(
        &root,
        "\"pool return\") echo '{\"returned\":false,\"running_processes\":2,\
         \"processes\":\"cargo node\",\"reason\":\"work is still running in the guest\",\
         \"hint\":\"--kill\"}' ; exit 1 ;;\n* ) exit 0 ;;",
    );
    let r = give_back(&bin, "proj-ab12", false).expect("exit 1 is an answer");
    assert!(r.still_running());
    let line = r.sentence("proj-ab12");
    assert!(
        line.contains("2 process(es)") && line.contains("cargo node"),
        "{line}"
    );
    assert!(said(&root)[0].ends_with("--sleep"));
}

/// `state reap --owners -` is fed the live ids on stdin, and what it kept is what the journal says.
#[test]
fn a_reap_is_fed_the_live_owners_on_stdin() {
    let root = tmp("reap");
    let bin = stub(
        &root,
        "\"state reap\") cat > /dev/null; echo '{\"dry_run\":false,\"reaped\":[\"r1\",\"r2\"],\
         \"kept\":[\"r3\"],\"kept_unextracted\":1}' ;;\n* ) exit 0 ;;",
    );
    let live = vec!["letibot:s-a:11".to_string(), "letibot:s-b:11".to_string()];
    let r = reap(&bin, &live).expect("a document");
    assert_eq!(r.reaped, vec!["r1", "r2"]);
    assert_eq!(r.kept_unextracted, 1);
    assert!(r.sentence().contains("reaped 2"));
    assert_eq!(said(&root)[0], "state reap --owners - --json");
}

/// The budget is handed over verbatim, and a gc that stopped short says why - it is not swallowed.
#[test]
fn gc_over_its_budget_is_reported_not_hidden() {
    let root = tmp("gc");
    let bin = stub(
        &root,
        "\"gc --state\") echo '{\"freed_bytes\":1073741824,\"removed\":[\"a\",\"b\"],\
         \"target_reached\":false,\"reason\":\"every remaining layer is younger than 14d\",\
         \"kept\":{}}' ;;\n* ) exit 0 ;;",
    );
    let g = gc_state(&bin, "150G", "14d").expect("a document");
    assert_eq!(g.freed_bytes, 1024 * 1024 * 1024);
    assert_eq!(g.target_reached, Some(false));
    let line = g.sentence();
    assert!(line.contains("STILL OVER"), "{line}");
    assert!(line.contains("younger than 14d"), "{line}");
    assert_eq!(
        said(&root)[0],
        "gc --state --budget 150G --max-age 14d --json"
    );
}

/// A VM row names the session its stamp says, and a `scope`/`work` word a head can print.
#[test]
fn state_ls_reads_a_vm_row_and_its_owner_session() {
    let root = tmp("ls");
    let bin = stub(
        &root,
        "\"state ls\") echo '{\"sessions\":[{\"name\":\"proj-ab12\",\
         \"owner\":\"letibot:s-1794:861706\",\"purpose\":\"reviewer\",\
         \"size\":412316860416,\"last_used\":\"2026-11-23T10:00:00Z\",\
         \"work\":\"out\",\"flagged\":false}]}' ;;\n* ) exit 0 ;;",
    );
    let ls = state_ls(&bin).expect("a document");
    assert_eq!(ls.sessions.len(), 1);
    let row = &ls.sessions[0];
    assert_eq!(row.owner_session(), Some("s-1794"));
    assert_eq!(row.name_or_project(), "proj-ab12");
    assert_eq!(row.work.as_deref(), Some("out"));
    assert_eq!(row.size, Some(412316860416));
    assert!(!row.flagged);
}

/// A box with no firecode is ONE answer - `Missing` - and not an error loop.
#[test]
fn no_firecode_here_is_missing_and_every_verb_says_so() {
    let root = tmp("missing");
    let nothing = root.join("no-such-firecode");
    let why = release(&nothing, Which::Name("proj-ab12")).expect_err("nothing is there");
    assert!(why.is_missing(), "{why:?}");
    assert!(why.sentence().contains("firecode"));
    assert!(
        take(&nothing, Path::new("/w"), Purpose::Reviewer)
            .expect_err("nothing is there")
            .is_missing()
    );
    assert!(
        gc_state(&nothing, "150G", "14d")
            .expect_err("nothing is there")
            .is_missing()
    );
}

/// A document this build cannot read is named with what it got, never defaulted to "fine".
#[test]
fn an_answer_of_the_wrong_shape_is_unreadable_and_says_what_it_got() {
    let root = tmp("shape");
    let bin = stub(
        &root,
        "\"state release\") echo '{\"released\":\"yes\"}' ;;\n* ) exit 0 ;;",
    );
    let why = release(&bin, Which::Name("proj-ab12")).expect_err("a bool is not a string");
    assert!(matches!(why, Failure::Unreadable { .. }), "{why:?}");
    assert!(why.sentence().contains("cannot read"), "{}", why.sentence());
}

/// The stamp is the whole of item 1: the owner spelling, the project key and the purpose, as flags
/// a newer firecode reads and as environment an older one does.
#[test]
fn a_stamp_names_its_owner_its_project_and_its_purpose() {
    let s = Stamp::here("s-1794", Purpose::Reviewer);
    assert_eq!(s.owner(), format!("letibot:s-1794:{}", std::process::id()));
    assert!(s.reuse, "a reviewer's VM is the one worth reusing");
    let flags = s.flags();
    assert!(
        flags
            .windows(2)
            .any(|w| w == ["--owner", s.owner().as_str()])
    );
    assert!(flags.windows(2).any(|w| w == ["--purpose", "reviewer"]));
    assert!(flags.iter().any(|f| f == "--reuse"));
    let env = s.env();
    assert!(
        env.iter()
            .any(|(k, v)| k == "FIRECODE_OWNER" && *v == s.owner())
    );
    assert!(
        env.iter()
            .any(|(k, v)| k == "FIRECODE_REUSABLE" && v == "1")
    );

    let mut one = Stamp::here("s-1794", Purpose::Ephemeral);
    one.project = Some("defc553f46c3".into());
    assert!(!one.reuse, "a one-shot is not reused");
    let flags = one.flags();
    assert!(
        flags
            .windows(2)
            .any(|w| w == ["--owner-project", "defc553f46c3"])
    );
    assert!(!flags.iter().any(|f| f == "--reuse"));
}

/// The project key is the store's own: twelve hex, and different for two different trees.
#[test]
fn the_project_key_is_the_store_spelling() {
    let root = tmp("project-key");
    let a = project_key(&root);
    assert_eq!(a.len(), 12);
    assert!(a.bytes().all(|b| b.is_ascii_hexdigit()), "{a}");
    let b = project_key(&root.join("nested"));
    assert_ne!(a, b);
}

/// A record of work firecode never copied out outlives the process that wrote it, and the reaper
/// finds it under the cache directory beside the copies.
#[test]
fn an_unextracted_record_is_found_again_and_can_be_forgotten() {
    let root = tmp("unextracted");
    let cache = root.join("cache");
    let rec = Unextracted {
        name: "child-1".into(),
        run: "firecode-20261014-191759-812644".into(),
        project: cache.join("child-1").display().to_string(),
        hint: Some("firecode extract … --bundle …".into()),
        attempts: 0,
        flagged: false,
        since_ms: 7,
    };
    assert!(rec.save(&cache));
    let found = Unextracted::all(&cache);
    assert_eq!(found, vec![rec.clone()]);
    let path = Unextracted::path_in(&cache, "child-1");
    assert!(path.is_file(), "the record is its own file");
    Unextracted::forget(&cache, "child-1");
    assert!(Unextracted::all(&cache).is_empty());
}

/// A different firecode version printing a banner before its document is still answering the
/// document - the JSON is the last line that parses.
#[test]
fn a_banner_before_the_document_is_not_a_wrong_shape() {
    let root = tmp("banner");
    let bin = stub(
        &root,
        "\"state ls\") echo '[firecode] state'; echo '{\"sessions\":[]}' ;;\n* ) exit 0 ;;",
    );
    let ls = state_ls(&bin).expect("the document on the last line");
    assert!(ls.sessions.is_empty());
}

/// Item 5: one pass reaps, sleeps the idle pool, and retries the extraction a refused release left
/// exactly once - the second pass flags the run instead of extracting again.
#[test]
fn a_pass_reaps_sleeps_and_retries_one_extraction() {
    let root = tmp("pass");
    let cache = root.join("cache");
    let bin = stub(
        &root,
        "\"state reap\") cat > \"$ROOT/live\"; echo '{\"dry_run\":false,\"reaped\":[],\"kept\":[],\
         \"kept_unextracted\":0}' ;;\n\
         \"pool sleep\") echo '{\"slept\":[\"proj-ab12\"]}' ;;\n\
         \"extract \"*) echo '{\"extracted\":false,\"reason\":\"the guest is gone\"}' ;;",
    );
    let rec = Unextracted {
        name: "child-1".into(),
        run: "firecode-20261014-191759-812644".into(),
        project: cache.join("child-1").display().to_string(),
        hint: Some("firecode extract r1 --bundle b".into()),
        attempts: 0,
        flagged: false,
        since_ms: 1,
    };
    assert!(rec.save(&cache));

    let pass = reap_pass(&bin, &["s-a".to_string()], &cache);
    assert!(pass.lines.iter().any(|l| l.contains("reaped")), "{pass:?}");
    assert!(
        pass.lines.iter().any(|l| l.contains("proj-ab12")),
        "{pass:?}"
    );
    let live = std::fs::read_to_string(root.join("live")).unwrap_or_default();
    assert_eq!(live.trim(), "s-a", "the live ids are fed on stdin");
    let after = Unextracted::all(&cache);
    assert_eq!(after.len(), 1, "a failed extraction keeps its record");
    assert_eq!(after[0].attempts, 1);

    // The second pass makes no second attempt: it flags the run and tells a person.
    let second = reap_pass(&bin, &[], &cache);
    assert!(
        second
            .refusals
            .iter()
            .any(|l| l.contains("still holds work")),
        "{second:?}",
    );
    assert!(Unextracted::all(&cache)[0].flagged);
}

/// A pass on a box with no firecode is ONE line and no work: never an error loop.
#[test]
fn a_pass_with_no_firecode_says_one_line() {
    let root = tmp("pass-missing");
    let nothing = root.join("no-such-firecode");
    let pass = reap_pass(&nothing, &[], &root);
    assert_eq!(pass.lines.len(), 1, "{pass:?}");
    assert!(pass.lines[0].contains("firecode"), "{:?}", pass.lines);
    assert!(pass.refusals.is_empty());
}

/// The last gc's answer outlives the daemon that ran it, which is what `letibot holds` reads to
/// report a box that is still over its budget.
#[test]
fn the_last_gc_is_a_note_a_second_process_can_read() {
    let dir = tmp("gc-note");
    assert!(read_gc_note(&dir).is_none());
    let note = GcNote {
        at_ms: 1_700_000_000_000,
        gc: Gc {
            freed_bytes: 2048,
            removed: vec!["a".into()],
            target_reached: Some(false),
            reason: Some("every remaining layer is younger than 14d".into()),
            kept: serde_json::Value::Null,
        },
    };
    assert!(save_gc_note(&dir, &note));
    let back = read_gc_note(&dir).expect("the note it just wrote");
    assert_eq!(back, note);
    assert!(back.gc.sentence().contains("STILL OVER"));
}
