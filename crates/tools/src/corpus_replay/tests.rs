//! The sweep's own tests, over a corpus small enough to reason about by hand.
//!
//! The file is `crates/tools/tests/data/gate-corpus-tiny.json`, beside this test the way
//! `answer-key.tsv` sits beside the search test: a fixture a build needs is in the tree,
//! and the operator's real corpus is not in the tree at all.
//!
//! Every command here is written for the fixture. Nothing in this module is copied from
//! a store.

use super::*;

fn env() -> Surroundings {
    Surroundings {
        home: Some("/w".into()),
        workspace: Some("/w/project".into()),
        ..Surroundings::default()
    }
    .with_pinned_shell("test: the fixture shell is fixed")
}

fn tiny() -> Report {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/gate-corpus-tiny.json");
    sweep(&path, &env()).expect("the fixture corpus must sweep")
}

#[test]
fn a_sweep_counts_the_rows_the_tools_and_the_commands() {
    let r = tiny();
    assert_eq!(r.rows, 10, "ten decisions in the fixture");
    assert_eq!(r.other_tools, 1, "one `edit` row");
    assert_eq!(r.bash, 9);
    assert_eq!(r.no_command, 0);
    assert_eq!(r.then_unknown, 1, "the row with no baseline column");
}

/// The measurement the whole instrument exists for: what was not read then, and is now.
#[test]
fn a_command_the_record_calls_unresolved_and_today_reads_is_counted_as_fixed() {
    let r = tiny();
    // Four rows say "never resolved" — `cat $FILE`, the `cd && export && cargo` chain,
    // the `$(…)`, and the `tail -5 …` row, which carries no baseline column at all and is
    // therefore `then_unknown` rather than either. The other six were read then.
    assert_eq!(
        r.then_resolved, 5,
        "two greps, two cargo tests and the oracle row; the \
                                     `edit` is not a bash row and is not counted here"
    );
    assert!(
        r.fixed >= 1,
        "the `$PWD`/`export` chain is read now and was not then: {r:?}"
    );
    assert_eq!(
        r.broken, 0,
        "nothing this sweep reads less than the record did"
    );
    assert_eq!(r.now_resolved, r.bash - r.unresolved_now);
}

#[test]
fn the_construct_that_stays_unresolved_is_named_by_the_parser_not_by_a_regex() {
    let r = tiny();
    let pe = r
        .constructs
        .get("parameter_expansion")
        .expect("`cat $FILE` is a parameter expansion and nothing else");
    assert_eq!((pe.appears, pe.alone), (1, 1));
    let cs = r
        .constructs
        .get("command_substitution_value")
        .expect("`$(…)` is its own construct");
    assert_eq!((cs.appears, cs.alone), (1, 1));
}

/// The approval key, replayed: a person answered a shape once, and the next call of that
/// shape — and, separately, a call only a session grant could cover — are counted as
/// questions that need not have been asked.
#[test]
fn a_second_human_ask_is_counted_as_settled_by_the_approval_the_first_one_recorded() {
    let r = tiny();
    assert_eq!(
        r.human, 4,
        "a grep and three cargo tests, each answered by a person"
    );
    assert_eq!(
        r.human_by_shape, 1,
        "the second `cargo test --offline -p …` is the shape the first one was: {r:?}"
    );
    assert_eq!(
        r.human_by_grant, 1,
        "the `--lib` cargo test has a shape of its own, so only the grant can cover it"
    );
    assert_eq!(
        r.human_by_mode, 1,
        "a grep reads only, and a read is admitted at every point"
    );
    assert_eq!(r.human_settled, 3);
    assert_eq!(
        r.human_settled,
        r.human_by_grant + r.human_by_shape + r.human_by_mode,
        "the three settle arms are disjoint, as the gate's order makes them"
    );
}

/// The oracle was shown a reading. Where today's reading differs, the decision was taken
/// on something else — and that is the count an operator has to see before trusting the
/// gate's own record.
#[test]
fn an_oracle_row_whose_recorded_intents_differ_from_todays_reading_is_counted() {
    let r = tiny();
    assert_eq!(r.oracle, 1);
    assert_eq!(r.oracle_unparsed, 0);
    assert_eq!(
        r.oracle_intents_differ, 1,
        "the fixture records `network` for a grep"
    );
    assert_eq!(r.oracle_either, 1);
}

#[test]
fn the_summarys_two_bracketed_sets_are_read_the_way_the_summary_writes_them() {
    let rec = recorded("ask — intents [read_file unknown] over [host_other]").unwrap();
    assert_eq!(
        rec.intents,
        ["read_file".to_string(), "unknown".to_string()].into()
    );
    assert_eq!(rec.regions.unwrap(), ["host_other".to_string()].into());

    // A tier clause after the regions does not confuse it, and the unread marker does
    // not either.
    let rec = recorded(
        "ask — intents [execute_code] over [workspace] — ALWAYS ASK (some rule) \
         — the meaning was never resolved (1 construct(s))",
    )
    .unwrap();
    assert_eq!(rec.intents, ["execute_code".to_string()].into());
    assert_eq!(rec.regions.unwrap(), ["workspace".to_string()].into());

    // Not a baseline summary: said, not guessed.
    assert!(recorded("auto (a read inside the boundary)").is_none());
}

#[test]
fn the_record_says_first_that_a_command_was_never_read() {
    let row = |by: &str, baseline: Option<&str>| Row {
        verdict_by: by.into(),
        baseline: baseline.map(str::to_string),
        ..Row::default()
    };
    // Since 2026-10-02 the summary carries the marker.
    assert!(unread(&row(
        "gate:mode",
        Some(
            "ask — intents [read_file unknown] over [unbounded] — the meaning was never \
             resolved (1 construct(s); each scope is the widest it could reach)"
        )
    )));
    // Before that date the verdict token WAS the refusal, and there was no marker — the
    // spelling that made 514 rows on lab2x1 look like a regression.
    assert!(unread(&row(
        "boundary:normaliser",
        Some("not_run — intents [inspect read_file unknown] over [host_other]")
    )));
    assert!(unread(&row(
        "gate:mode",
        Some("not_run — intents [read_file] over [home]")
    )));
    // And a command that WAS read says so by saying nothing.
    assert!(!unread(&row(
        "gate:mode",
        Some("ask — intents [read_file] over [workspace]")
    )));
    // A row with no reading at all is unknown, not unresolved.
    assert!(!unread(&row("gate:permission", None)));
}

#[test]
fn a_missing_corpus_is_an_error_and_not_an_empty_report() {
    let e = sweep(
        std::path::Path::new("/nonexistent/gate-corpus.json"),
        &env(),
    )
    .expect_err("a path that is not there must not read as a corpus of nothing");
    assert!(e.contains("/nonexistent/gate-corpus.json"), "{e}");
}

/// A file that is not an array of decisions is refused rather than half-read: a sweep
/// that reported on a truncated corpus would be a number nobody could act on.
#[test]
fn a_file_that_is_not_a_decision_array_is_refused() {
    let path = std::env::temp_dir().join("gate-corpus-not-an-array.json");
    std::fs::write(&path, "{\"rows\": []}").unwrap();
    let e = sweep(&path, &env()).expect_err("an object is not a corpus");
    assert!(e.contains("gate-corpus-not-an-array.json"), "{e}");
    let _ = std::fs::remove_file(&path);
}
