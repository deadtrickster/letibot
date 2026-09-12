//! The six clauses of §8.1, as acceptance tests over the whole runtime.
//!
//! The unit tests inside the crate check the pieces. These check the thing the
//! plan actually promises: **what the model sees**. So every assertion here is
//! against `ToolResult::render()` — the exact bytes that become a
//! `TranscriptItem::ToolResult` — and not against an internal field, because a
//! clause that holds in a struct and not in the prompt has not held.

use letibot_tools::builtins::retrieval::{Retrieval, Unavailable};
use letibot_tools::events::ToolEvent;
use letibot_tools::result::{Envelope, Propagation, propagate};
use letibot_tools::schema::Access;
use letibot_tools::spill::{FixedBudget, MemoryStore, Spiller};
use letibot_tools::testing::{
    Scripted, harness, harness_with_retrieval, harness_with_spiller, writable_harness,
    writable_harness_with_budget, writable_harness_with_gate,
};
use letibot_transcript::ToolOutcome;

// ---------------------------------------------------------------------------
// Clause 1 — a miss is self-correcting in the SAME call.
// ---------------------------------------------------------------------------

#[test]
fn clause1_a_scoped_grep_that_misses_reports_where_the_term_does_occur() {
    let mut h = harness();
    let seen = h
        .call("grep", r#"{"pattern":"cache_prompt","path":"src"}"#)
        .render();

    // The three things the model needs, all in this one call: that the scope was
    // empty, where the term is, and the matching lines themselves.
    assert!(seen.contains("nothing matches under `src`"), "{seen}");
    assert!(seen.contains("docs/notes.md"), "{seen}");
    assert!(seen.contains("cache_prompt"), "{seen}");
    // And it is not dressed as a failure: a redirect is a result.
    assert_eq!(Envelope::classify(&seen), None, "{seen}");
}

#[test]
fn clause1_a_glob_that_misses_returns_the_surrounding_listing_and_what_would_have_matched() {
    let mut h = harness();
    let seen = h
        .call("glob", r#"{"pattern":"src/parser/**/*.rs"}"#)
        .render();

    assert!(seen.contains("matched nothing as written"), "{seen}");
    // What would have matched.
    assert!(seen.contains("lib.rs"), "{seen}");
    // The surrounding listing: `src/parser` does not exist and `src/util` does.
    assert!(seen.contains("util"), "{seen}");
}

#[test]
fn clause1_a_too_strict_anchor_is_relaxed_and_the_relaxation_is_reported() {
    let mut h = harness();
    let seen = h
        .call("grep", r#"{"pattern":"^\\s*fn parse_args\\("}"#)
        .render();

    assert!(seen.contains("bare identifier"), "{seen}");
    assert!(seen.contains("parse_args"), "{seen}");
    assert!(seen.contains("src/lib.rs:"), "{seen}");
    // §9.4: the rewrite is visible. The model is told the original still fails.
    assert!(seen.contains("still matches nothing"), "{seen}");
}

#[test]
fn clause1_a_miss_carries_more_that_is_actionable_than_a_hit_does() {
    // The property behind the clause, stated as a measurement rather than a
    // wording: a miss must not be *smaller* than a hit, because "not found" being
    // cheap is exactly what makes the model guess again.
    let mut h = harness();
    let hit = h.call("read", r#"{"path":"README.md"}"#).render();
    let miss = h.call("read", r#"{"path":"READ_ME.md"}"#).render();
    assert!(
        miss.len() > hit.len(),
        "a miss returned {} bytes and the hit returned {}",
        miss.len(),
        hit.len()
    );
    assert!(miss.contains("README.md"), "{miss}");
}

// ---------------------------------------------------------------------------
// Clause 2 — malformed input is salvaged, not rejected.
// ---------------------------------------------------------------------------

#[test]
fn clause2_a_leaked_function_envelope_is_executed_and_the_repair_is_stated() {
    // oracle's measured case: this shape was 33% of calls dropped.
    let mut h = harness();
    let r = h.call(
        "read",
        "<function=read>{'file_path': 'src/lib.rs', 'limit': '2',}</function>",
    );
    assert!(r.is_grounded(), "{:?} {}", r.outcome, r.payload);
    let seen = r.render();
    assert!(
        seen.contains("parse_args") || seen.contains("use std::io"),
        "{seen}"
    );

    // Four separate repairs, and every one of them said out loud: the model that
    // is never told keeps spelling it that way for the rest of the session.
    let codes: Vec<&str> = r.repairs.iter().map(|x| x.code).collect();
    for want in [
        "envelope.function",
        "json.quotes",
        "key.renamed",
        "type.coerced",
    ] {
        assert!(codes.contains(&want), "missing {want} in {codes:?}");
    }
    assert!(seen.contains("[repaired]"), "{seen}");
}

#[test]
fn clause2_what_cannot_be_read_at_all_gets_the_parameter_list_rather_than_a_complaint() {
    let mut h = harness();
    let seen = h.call("read", "just read the config file please").render();
    assert_eq!(Envelope::classify(&seen), Some("TOOL_ERROR"), "{seen}");
    assert!(seen.contains("path"), "{seen}");
    assert!(seen.contains("required"), "{seen}");
}

// ---------------------------------------------------------------------------
// Clause 3 — outcome is a closed vocabulary and abstention is not success.
// ---------------------------------------------------------------------------

#[test]
fn clause3_abstention_is_structurally_distinct_in_what_the_model_sees() {
    let mut h = harness_with_retrieval(Scripted::no_coverage());
    let r = h.call("ask_corpus", r#"{"question":"does it cover bats?"}"#);
    let seen = r.render();

    assert!(matches!(r.outcome, ToolOutcome::Abstained { .. }));
    assert_eq!(Envelope::classify(&seen), Some("NO_RESULT"), "{seen}");
    assert!(seen.starts_with("<<<NO_RESULT "), "{seen}");
    assert!(
        seen.trim_end()
            .ends_with(&Envelope::no_result(&r.call_id).close())
    );
    assert!(seen.contains("may be cited"), "{seen}");

    // And an answered call is not in that envelope, so the two are distinguishable
    // by shape rather than by reading the prose.
    let mut h = harness_with_retrieval(Scripted::answering());
    let ok = h
        .call("ask_corpus", r#"{"question":"what is the ledger?"}"#)
        .render();
    assert_eq!(Envelope::classify(&ok), None, "{ok}");
}

#[test]
fn clause3_abstention_propagates_and_a_caller_cannot_report_ok() {
    let mut h = harness_with_retrieval(Scripted::no_coverage());
    let a = h.call("ask_corpus", r#"{"question":"one"}"#).outcome;
    let b = h.call("ask_code", r#"{"question":"two"}"#).outcome;

    match propagate(&[a, b]) {
        Propagation::Must(ToolOutcome::Abstained { .. }) => {}
        other => panic!("a caller whose calls all abstained must not report Ok: {other:?}"),
    }
}

#[test]
fn clause3_an_empty_search_abstains_rather_than_returning_an_empty_success() {
    let mut h = harness();
    let r = h.call("grep", r#"{"pattern":"quokka_sentinel"}"#);
    assert!(
        matches!(r.outcome, ToolOutcome::Abstained { .. }),
        "an empty result is not a result: {:?}",
        r.outcome
    );
    // It still says what was searched — clause 1 does not switch off for clause 3.
    assert!(r.render().contains("relaxations tried"), "{}", r.render());
}

// ---------------------------------------------------------------------------
// Clause 4 — read/write declared in the schema, not decided per call.
// ---------------------------------------------------------------------------

#[test]
fn clause4_every_built_in_declares_read_and_none_of_them_reaches_the_gate() {
    struct Exploding;
    impl letibot_tools::runtime::Gate for Exploding {
        fn admit(
            &mut self,
            call: &letibot_tools::runtime::GateCall<'_>,
        ) -> letibot_tools::runtime::GateDecision {
            panic!(
                "{} ({:?}) reached the gate and it declares read access",
                call.name, call.access
            );
        }
    }

    let mut h = harness();
    h.rt = h.rt.with_gate(Box::new(Exploding));
    for (tool, args) in [
        ("read", r#"{"path":"README.md"}"#),
        ("grep", r#"{"pattern":"letibot"}"#),
        ("glob", r#"{"pattern":"**/*.rs"}"#),
        ("ask_code", r#"{"question":"anything"}"#),
        ("ask_corpus", r#"{"question":"anything"}"#),
        ("read_spill", r#"{"hash":"none"}"#),
    ] {
        h.call(tool, args);
    }
}

#[test]
fn clause4_the_access_class_is_not_shown_to_the_model() {
    // It is policy input (§11.3), not prompt. A model that can read it can argue
    // about it.
    let reg = letibot_tools::read_only_tools(std::sync::Arc::new(Unavailable)).unwrap();
    for json in reg.tools_json() {
        assert!(!json.contains("\"access\""), "{json}");
    }
}

// ---------------------------------------------------------------------------
// Clause 5 — output is bounded and spilled, never truncated.
// ---------------------------------------------------------------------------

#[test]
fn clause5_a_large_output_is_bounded_recoverable_and_never_silently_cut() {
    let mut h = harness_with_spiller(Spiller::new(
        Box::new(FixedBudget(1_000)),
        Box::new(MemoryStore::new()),
    ));
    let r = h.call("read", r#"{"path":"big.txt"}"#);
    let spill = r
        .spill
        .clone()
        .expect("a 30 KB file under a 1 KB cap must spill");

    let seen = r.render();
    assert!(seen.contains("Omitted"), "{seen}");
    assert!(seen.contains(&spill.hash), "{seen}");
    // The inline body respects the cap; the notice and the repair lines are the
    // envelope around it.
    assert!(r.payload.len() <= 1_000, "{} bytes inline", r.payload.len());

    // Nothing was lost: the whole original is behind the locator.
    let whole = h.rt.spiller.store.get(&spill.hash, None).unwrap();
    assert_eq!(whole.len(), spill.full_bytes);
    assert!(
        String::from_utf8(whole)
            .unwrap()
            .contains("filler line 1999")
    );
}

#[test]
fn clause5_unset_is_a_no_op_rather_than_a_default_nobody_chose() {
    // D6: `max_inline_bytes` keeps no default.
    let mut h = harness();
    let r = h.call("read", r#"{"path":"big.txt"}"#);
    assert!(r.spill.is_none());
    assert!(r.payload.len() > 30_000, "{} bytes", r.payload.len());
}

// ---------------------------------------------------------------------------
// Clause 6 — the description says how to use the tool.
// ---------------------------------------------------------------------------

#[test]
fn clause6_no_built_in_description_says_what_the_data_contains() {
    let reg = letibot_tools::read_only_tools(std::sync::Arc::new(Unavailable)).unwrap();
    for s in reg.schemas() {
        assert_eq!(
            letibot_tools::lint_description(&s.description),
            vec![],
            "{}",
            s.name
        );
    }
}

// ---------------------------------------------------------------------------
// The events, which is what W9 owed T13.4 an answer about.
// ---------------------------------------------------------------------------

#[test]
fn the_lifecycle_events_carry_what_a_head_needs() {
    let mut h = harness_with_spiller(Spiller::new(
        Box::new(FixedBudget(1_000)),
        Box::new(MemoryStore::new()),
    ));
    h.call("read", "<function=read>{'file_path': 'big.txt'}</function>");

    let started = h
        .sink
        .events
        .iter()
        .find(|e| matches!(e, ToolEvent::Started { .. }))
        .expect("a started event");
    match started {
        ToolEvent::Started {
            turn_id, access, ..
        } => {
            assert_eq!(turn_id, "turn_1");
            assert_eq!(*access, Access::Read);
        }
        _ => unreachable!(),
    }

    let finished = h
        .sink
        .events
        .iter()
        .find(|e| matches!(e, ToolEvent::Finished { .. }))
        .expect("a finished event");
    match finished {
        ToolEvent::Finished {
            inline_bytes,
            full_bytes,
            spill,
            repairs,
            outcome,
            ..
        } => {
            assert_eq!(*outcome, ToolOutcome::Ok);
            // The two byte counts differ, which is the fact a single `bytes` field
            // could not carry.
            assert!(full_bytes > inline_bytes, "{full_bytes} vs {inline_bytes}");
            assert!(spill.is_some(), "a spilled call must name its locator");
            assert!(*repairs > 0, "the repairs are countable by a head");
        }
        _ => unreachable!(),
    }
}

#[test]
fn a_refused_call_never_reports_that_it_started() {
    // The ordering a head depends on: `ToolStarted` means the tool ran.
    struct Nobody;
    impl Retrieval for Nobody {
        fn ask(
            &self,
            _k: letibot_tools::builtins::retrieval::RetrievalKind,
            _q: &letibot_tools::builtins::retrieval::RetrievalQuery,
        ) -> Result<
            letibot_tools::builtins::retrieval::RetrievalAnswer,
            letibot_tools::builtins::retrieval::RetrievalError,
        > {
            unreachable!()
        }
        fn describe(&self) -> String {
            "nobody".into()
        }
    }
    let mut h = harness_with_retrieval(std::sync::Arc::new(Nobody));
    h.call("no_such_tool", "{}");
    assert_eq!(h.sink.kinds(), vec!["ToolFinished"]);
}

#[test]
fn the_transcript_row_carries_the_envelope_the_model_saw() {
    // The seam to the turn engine. `TranscriptItem::ToolResult` is what the next
    // prompt replays, so the payload it carries must be the *rendered* result and
    // not the raw body: were it the raw body, the abstention envelope would exist
    // for one turn and vanish at the next prefill, which is §8.2's failure with an
    // extra step.
    let mut h = harness_with_retrieval(Scripted::no_coverage());
    let r = h.call("ask_corpus", r#"{"question":"anything"}"#);
    match letibot_tools::runtime::ToolRuntime::transcript_item(&r) {
        letibot_transcript::TranscriptItem::ToolResult {
            outcome, payload, ..
        } => {
            assert!(matches!(outcome, ToolOutcome::Abstained { .. }));
            assert_eq!(Envelope::classify(&payload), Some("NO_RESULT"), "{payload}");
        }
        other => panic!("expected a tool result row, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// The write tools (W10), against the same six clauses.
//
// Separated because these are the first tools that can change the operator's
// tree, so for them the clause tests are also the safety tests: every one
// asserts what is **on disk** afterwards, through `read_file`, and never through
// what the tool said about itself.
// ---------------------------------------------------------------------------

#[test]
fn clause1_an_edit_that_misses_reports_the_text_that_is_actually_there() {
    // The measured shape, one tool over: the guess is refused *and* what would
    // have been correct comes back, so the model corrects itself in the same call.
    let mut h = writable_harness();
    h.write_file("a.rs", "fn main() {\n\tlet ledger = 1;\n}\n");
    h.call("read", r#"{"path":"a.rs"}"#);
    let seen = h
        .call(
            "edit",
            r#"{"path":"a.rs","old_string":"    let ledger = 1;","new_string":"    let ledger = 2;"}"#,
        )
        .render();

    // Which relaxation, where, and the literal bytes to copy.
    assert!(seen.contains("indentation"), "{seen}");
    assert!(seen.contains("lines 2–2"), "{seen}");
    assert!(
        seen.contains("---8<---\n\tlet ledger = 1;\n--->8---"),
        "{seen}"
    );
    assert!(seen.contains("nothing was written"), "{seen}");
    // And the file is untouched, which is the assertion the wording cannot fake.
    assert_eq!(h.read_file("a.rs"), "fn main() {\n\tlet ledger = 1;\n}\n");
}

#[test]
fn clause1_more_than_one_match_reports_every_line_rather_than_asking_for_more_context() {
    let mut h = writable_harness();
    h.write_file(
        "b.rs",
        "let n = 0;\nfoo();\nlet n = 0;\nbar();\nlet n = 0;\n",
    );
    h.call("read", r#"{"path":"b.rs"}"#);
    let seen = h
        .call(
            "edit",
            r#"{"path":"b.rs","old_string":"let n = 0;","new_string":"let n = 1;"}"#,
        )
        .render();
    assert!(seen.contains("occurs 3 times"), "{seen}");
    for line in ["at line 1:", "at line 3:", "at line 5:"] {
        assert!(seen.contains(line), "{line} missing from {seen}");
    }
    // The surrounding lines are what tell the three apart, so they are shown.
    assert!(seen.contains("foo();") && seen.contains("bar();"), "{seen}");
    assert!(seen.contains("replace_all"), "{seen}");
    assert_eq!(
        h.read_file("b.rs"),
        "let n = 0;\nfoo();\nlet n = 0;\nbar();\nlet n = 0;\n"
    );
}

#[test]
fn clause2_a_malformed_edit_call_is_salvaged_and_the_repair_is_said_out_loud() {
    // Single quotes, unquoted keys, and `replace_all` as the string models
    // actually emit. Salvaged — and the repairs are rendered, so the next call is
    // not malformed the same way.
    let mut h = writable_harness();
    h.write_file("c.rs", "let n = 0;\nlet n = 0;\n");
    h.call("read", r#"{"path":"c.rs"}"#);
    let r = h.call(
        "edit",
        "{path: 'c.rs', 'old_string': 'let n = 0;', new_string: 'let n = 1;', \
         replace_all: \"true\"}",
    );
    let seen = r.render();
    assert!(r.is_grounded(), "{seen}");
    assert!(!r.repairs.is_empty(), "the repairs must be carried");
    assert!(seen.contains("[repaired]"), "{seen}");
    assert_eq!(h.read_file("c.rs"), "let n = 1;\nlet n = 1;\n");
}

#[test]
fn clause3_a_refused_write_is_never_ok_and_never_an_abstention() {
    // Three refusals, three outcome classes, and none of them is `Ok`.

    // Nothing decided: nobody was asked, and the *class* says so.
    let mut closed = writable_harness_with_gate(None);
    closed.call("read", r#"{"path":"README.md"}"#);
    let r = closed.call("write", r#"{"path":"README.md","content":"x"}"#);
    assert!(
        matches!(r.outcome, ToolOutcome::NotRun { .. }),
        "{:?}",
        r.outcome
    );

    // A decision was made: `Denied`, carrying the request id.
    let mut denied = writable_harness_with_gate(Some(deny_all()));
    denied.call("read", r#"{"path":"README.md"}"#);
    let r = denied.call("write", r#"{"path":"README.md","content":"x"}"#);
    assert!(
        matches!(r.outcome, ToolOutcome::Denied { .. }),
        "{:?}",
        r.outcome
    );

    // A tool-level refusal: the call ran and could not do what was asked.
    let mut h = writable_harness();
    let r = h.call("write", r#"{"path":"README.md","content":"x"}"#);
    assert!(
        matches!(r.outcome, ToolOutcome::Failed { .. }),
        "{:?}",
        r.outcome
    );
    assert!(!r.is_grounded());

    // Abstention is for "nothing to say about this", and a write never abstains:
    // a write either happened or it did not.
    assert_eq!(Envelope::classify(&r.render()), Some("TOOL_ERROR"));
}

#[test]
fn clause4_both_write_tools_declare_write_and_a_write_always_reaches_the_gate() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let reg = letibot_tools::coder_tools(Arc::new(Unavailable)).unwrap();
    for s in reg.schemas() {
        let want = if matches!(s.name.as_str(), "write" | "edit") {
            Access::Write
        } else {
            Access::Read
        };
        assert_eq!(s.access, want, "{} declares the wrong access", s.name);
        // Still not shown to the model: policy input, not prompt.
        assert!(!s.prompt_json().contains("access"), "{}", s.prompt_json());
    }

    // "A write tool always prompts unless policy says otherwise" as a
    // control-flow fact: a counting gate, and a write call that must reach it.
    struct Counting(Arc<AtomicUsize>);
    impl letibot_tools::runtime::Gate for Counting {
        fn admit(
            &mut self,
            _call: &letibot_tools::runtime::GateCall<'_>,
        ) -> letibot_tools::runtime::GateDecision {
            self.0.fetch_add(1, Ordering::Relaxed);
            letibot_tools::runtime::GateDecision::Admit
        }
    }
    let n = Arc::new(AtomicUsize::new(0));
    let mut h = writable_harness_with_gate(Some(Box::new(Counting(n.clone()))));
    h.call("read", r#"{"path":"README.md"}"#);
    assert_eq!(n.load(Ordering::Relaxed), 0, "a read never prompts");
    h.call("write", r#"{"path":"README.md","content":"x\n"}"#);
    assert_eq!(n.load(Ordering::Relaxed), 1, "a write always does");
}

#[test]
fn clause5_a_write_tools_output_spills_like_everything_else() {
    // The refusal that hands back a whole file is exactly the payload clause 5
    // exists for: bounded, recoverable, and never silently cut.
    let mut h = writable_harness_with_budget(Spiller::new(
        Box::new(FixedBudget(900)),
        Box::new(MemoryStore::new()),
    ));
    let r = h.call("write", r#"{"path":"big.txt","content":"gone\n"}"#);
    let spill = r.spill.as_ref().expect("a 26 KB refusal must spill");
    assert!(spill.full_bytes > spill.inline_bytes);

    let back = h.call("read_spill", &format!(r#"{{"hash":"{}"}}"#, spill.hash));
    assert!(
        back.render().contains("filler line 1999"),
        "the rest is fetchable"
    );
    // …and nothing was written, which is what the refusal said.
    assert!(h.read_file("big.txt").contains("filler line 0"));
}

#[test]
fn clause6_the_write_tool_descriptions_say_only_what_this_code_does() {
    // opencode's `edit.txt` promises a read-before-write error and two error
    // strings that do not exist anywhere in its code; grok-build strips a sentence
    // from its served description when the behaviour is off, so *"the model is
    // never told a rule that isn't running"*. Both promises here are behaviours,
    // and this asserts them by making the tool keep them.
    use std::sync::Arc;
    let reg = letibot_tools::coder_tools(Arc::new(Unavailable)).unwrap();
    let edit = reg
        .schemas()
        .into_iter()
        .find(|s| s.name == "edit")
        .unwrap();
    assert_eq!(
        letibot_tools::schema::lint_description(&edit.description),
        vec![]
    );
    assert!(edit.description.contains("Read the file first"));
    assert!(
        edit.description
            .contains("exactly once unless `replace_all`")
    );

    let mut h = writable_harness();
    // …the read-before-write promise.
    assert!(
        !h.call(
            "edit",
            r#"{"path":"README.md","old_string":"letibot","new_string":"x"}"#
        )
        .is_grounded()
    );
    // …and the exactly-once promise.
    h.write_file("d.rs", "x\nx\n");
    h.call("read", r#"{"path":"d.rs"}"#);
    assert!(
        !h.call(
            "edit",
            r#"{"path":"d.rs","old_string":"x","new_string":"y"}"#
        )
        .is_grounded()
    );
}

#[test]
fn a_write_is_atomic_and_leaves_no_temporary_behind() {
    // Neither prior harness does this: opencode's `writeWithDirs` and
    // grok-build's `fs.write_file` are both plain truncating writes, so a crash
    // mid-write publishes a half file and a fresh create loses the mode. This
    // asserts the two observable consequences of temp-plus-rename: the mode
    // survives, and the directory is left clean.
    let mut h = writable_harness();
    h.write_file("script.sh", "#!/bin/sh\necho one\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            h.root().join("script.sh"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    h.call("read", r#"{"path":"script.sh"}"#);
    let r = h.call(
        "edit",
        r#"{"path":"script.sh","old_string":"echo one","new_string":"echo two"}"#,
    );
    assert!(r.is_grounded(), "{}", r.render());
    assert_eq!(h.read_file("script.sh"), "#!/bin/sh\necho two\n");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(h.root().join("script.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755, "an edit must not disarm an executable");
    }

    let leftovers: Vec<String> = std::fs::read_dir(h.root())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.contains("letibot-") && n.ends_with(".tmp"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "temporaries left behind: {leftovers:?}"
    );
}

#[test]
fn the_head_is_handed_both_sides_and_the_model_is_not() {
    // The two halves of the hand-off decision, asserted together because they are
    // one decision: `crates/ui`'s differ needs both sides, and the prompt must not
    // carry a diff of an edit the model authored.
    let mut h = writable_harness();
    h.call("read", r#"{"path":"README.md"}"#);
    let r = h.call(
        "edit",
        r#"{"path":"README.md","old_string":"a harness","new_string":"a local-first harness"}"#,
    );
    let e = r.edit.as_ref().expect("the head gets both sides");
    assert_eq!(e.before, "letibot\na harness\n");
    assert_eq!(e.after, "letibot\na local-first harness\n");
    assert!(!e.before.contains('\r') && !e.after.contains('\r'));
    assert_eq!(e.changed.first, 2);

    // Nothing of a diff is in the prompt bytes; what is there is where it landed.
    let seen = r.render();
    assert!(seen.contains("1 replacement(s)"), "{seen}");
    assert!(seen.contains("     2| a local-first harness"), "{seen}");
    assert!(
        !seen.contains("\n-"),
        "no diff markers in the prompt: {seen}"
    );
}

#[test]
fn the_never_write_list_is_refused_before_any_adjudicator_sees_it() {
    // §11.9's rule: a `deny` row is evaluated before dispatch and no adjudicator
    // can override it. `writable_harness` attaches one that says yes to
    // everything, and it does not get asked.
    let mut h = writable_harness();
    h.write_file(".ssh/config", "Host x\n");
    h.call("read", r#"{"path":".ssh/config"}"#);
    let r = h.call(
        "edit",
        r#"{"path":".ssh/config","old_string":"Host x","new_string":"Host y"}"#,
    );
    assert!(
        matches!(r.outcome, ToolOutcome::Denied { .. }),
        "{:?}",
        r.outcome
    );
    assert!(r.render().contains("never-write list"), "{}", r.render());
    assert_eq!(h.read_file(".ssh/config"), "Host x\n");
}

/// An adjudicator that denies, for the outcome-class test.
fn deny_all() -> Box<dyn letibot_tools::runtime::Gate> {
    use letibot_tools::adjudicate::{AdjudicatedGate, AdjudicationDecision, AskAdjudicator};
    Box::new(AdjudicatedGate::new(Box::new(AskAdjudicator::new(
        "test",
        |req: &letibot_tools::adjudicate::AdjudicationRequest| {
            Some(AdjudicationDecision::selected(
                req,
                "deny_and_tell",
                "human:test",
                "not that file, not tonight",
            ))
        },
    ))))
}
