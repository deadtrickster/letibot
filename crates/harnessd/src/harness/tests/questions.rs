//! Questions a session asks and the rows its answers become.

use super::*;

/// The answer row carries what the person said, and nothing of ours: a label, an index or a harness sentence glued to
/// the front would be a citable utterance of the wrong thing, and the trail cites this row.
#[test]
fn the_answer_row_is_the_persons_words_and_nothing_else() {
    // A choice is the OPTION'S TEXT — the label they were shown and picked. An index is
    // not something anybody said.
    assert_eq!(
        operator_answer_text(&asked(QuestionAnswer::choosing(1))).as_deref(),
        Some("sqlite")
    );
    // A note is theirs too, and it qualifies the choice, so it rides with it.
    assert_eq!(
        operator_answer_text(&asked(
            QuestionAnswer::choosing(0).with_note("only for the CUDA box")
        ))
        .as_deref(),
        Some("postgres\nonly for the CUDA box")
    );
    // Typed words are the whole answer.
    assert_eq!(
        operator_answer_text(&asked(QuestionAnswer::free("neither — split it in two"))).as_deref(),
        Some("neither — split it in two")
    );
    // And an abstention is the act, in the only word the wire carries for it.
    assert_eq!(
        operator_answer_text(&asked(QuestionAnswer::abstaining())).as_deref(),
        Some("abstain")
    );
    // **An index nobody offered has nothing of theirs in it.** The tool refuses one of
    // those, so this is the belt to that pair of braces rather than a state a session
    // reaches — and returning words here would be inventing a sentence for them.
    assert_eq!(
        operator_answer_text(&asked(QuestionAnswer::choosing(7))),
        None
    );
}

/// The two rows: the question is the session's and the answer is the operator's. The speakers are the whole of what an
/// `ALLOW <n>` rests on, so they are asserted on the rows and not on a rendering.
#[test]
fn the_question_row_is_the_sessions_and_the_answer_row_is_the_operators() {
    let rows = asked_rows(&asked(QuestionAnswer::choosing(1)), "sqlite");
    let [question, answer] = rows;
    let text = |item: &TranscriptItem| match item {
        TranscriptItem::User { parts, .. } => parts
            .iter()
            .map(|p| match p {
                UserPart::Text { text } => text.clone(),
                other => format!("{other:?}"),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        other => panic!("not a user row: {other:?}"),
    };
    assert!(
        matches!(
            question,
            TranscriptItem::User {
                speaker: letibot_transcript::Speaker::Agent,
                ..
            }
        ),
        "the question must never read back as the operator's words: {question:?}"
    );
    let asked_text = text(&question);
    assert!(
        asked_text.contains("which database should the migration target?")
            && asked_text.contains("options offered:\n- postgres\n- sqlite"),
        "the question row must name the question and the options it offered: {asked_text}"
    );
    assert!(
        matches!(
            answer,
            TranscriptItem::User {
                speaker: letibot_transcript::Speaker::Operator,
                ..
            }
        ),
        "the answer row must be the person's: {answer:?}"
    );
    assert_eq!(text(&answer), "sqlite");
}

/// A job this daemon never ran is answered from the record, not refused: three claims, and the third is the one a reader
/// acts on. The state word is the row's own, the bytes are admitted lost, and a redirect names the file that holds them.
#[test]
fn a_recorded_job_says_what_happened_and_where_its_bytes_went() {
    let row = |state: &str, redirect: Option<&str>| JobRecord {
        handle: "j7".into(),
        command: "cargo test --release".into(),
        how: String::new(),
        state: state.into(),
        produced: 4_096,
        elapsed_ms: 8_000,
        redirect: redirect.map(str::to_string),
    };
    let said = recorded_job_lines(&row("exited 0", None)).join("\n");
    assert!(said.contains("exited 0"), "{said}");
    assert!(said.contains("8.0s"), "the duration is the row's: {said}");
    assert!(said.contains("4096 bytes"), "{said}");
    assert!(
        said.contains("not kept"),
        "the bytes are lost and the window must say so rather than look empty: {said}"
    );
    // **A word this function does not know is REPEATED, not interpreted.** A job whose
    // state is `not run (could not join its scope)` must not read here as one that ran,
    // which is why the sentence says *not in this daemon's table* and not *it ran*.
    let never = recorded_job_lines(&row("not run (could not join its scope)", None)).join("\n");
    assert!(
        never.contains("not run (could not join its scope)"),
        "{never}"
    );
    // **And the file, when the row knows it** — R41's other half: the pane cannot read the
    // bytes, so the one useful thing it can do is say where they are.
    let redirected = recorded_job_lines(&row("exited 0", Some("/tmp/build.log"))).join("\n");
    assert!(
        redirected.contains("/tmp/build.log"),
        "a redirected job's file is not named: {redirected}"
    );
}

/// A row announced through a capturing sink is filled by the pairing rule, and body-less without it: a sink announces on
/// `emit` and carries the id, so a caller that drops the sink has announced a row it never filled, which is what the
/// daemon's view then serves to every head and every `/resync`.
#[test]
fn an_announced_row_is_filled_by_the_pairing_rule_and_body_less_without_it() {
    use super::*;
    let item = || TranscriptItem::System {
        text: "the summary instruction".into(),
        origin: letibot_transcript::SystemOrigin::Update,
    };
    let announce = |sink: &mut CapturingSink| {
        sink.emit(letibot_turn::TurnEvent::TranscriptAppended {
            item_id: "s#t0.9".into(),
            kind: "system",
            ledger_head: "beef".into(),
            tokens: 3,
        });
    };

    // The defect: announced, sink dropped, no body. The view holds the row.
    let hub = Hub::new("s");
    let mut sink = CapturingSink::new(hub.clone());
    announce(&mut sink);
    drop(sink);
    let snap = hub.snapshot();
    assert_eq!(snap.items.len(), 1, "the announcement reached the log");
    assert!(
        snap.items[0].item.is_none(),
        "the premise: nothing filled it"
    );

    // The rule: same announcement, paired. The row carries its body.
    let hub = Hub::new("s");
    let mut sink = CapturingSink::new(hub.clone());
    announce(&mut sink);
    reconcile_rows(&hub, &mut sink, std::slice::from_ref(&item()));
    let snap = hub.snapshot();
    assert_eq!(snap.items.len(), 1);
    assert!(
        snap.items[0].item.is_some(),
        "announced and never filled: {:?}",
        snap.items[0].item_id
    );
}

/// A round's ending goes straight out of a sink that was not asked to hold it. Only the round loop
/// may hold one: every other door's turn has that frame for its only terminal event, so a held one a
/// head never hears is a compaction whose turn stays `Responding` for ever.
#[test]
fn a_capturing_sink_forwards_a_rounds_ending_unless_the_loop_holds_it() {
    fn ending() -> letibot_turn::TurnEvent {
        letibot_turn::TurnEvent::TurnFinished {
            turn_id: "t1".into(),
            finish_reason: letibot_turn::FinishReason::Eos,
            // No call of this round is left to run, so the engine's own reading is an ending.
            proposed_calls: false,
            metrics: Box::new(letibot_turn::TurnMetrics {
                turn_id: "t1".into(),
                model: "m".into(),
                dialect_template_sha: "00".into(),
                ledger_head_sent: "ab".into(),
                prompt_tokens: 10,
                cached_tokens: 0,
                predicted_tokens: 1,
                prompt_tokens_server: 10,
                prompt_processed: 10,
                finish_reason: letibot_turn::FinishReason::Eos,
                prompt_ms: 1.0,
                predicted_ms: 1.0,
                draft_n: 0,
                draft_n_accepted: 0,
                id_slot: 0,
                n_busy_slots: None,
                prefix_check: letibot_turn::PrefixCheck::FirstTurn,
                cost: letibot_backend::TurnCost {
                    meter: letibot_backend::Meter::WallClock,
                    prompt_tokens: 10,
                    cached_tokens: 0,
                    generated_tokens: 1,
                    wall_ms: 2,
                    micros_usd: None,
                },
                wall_ms: 2,
                first_byte_ms: None,
            }),
        }
    }
    fn endings(hub: &Hub) -> Vec<bool> {
        hub.retained()
            .iter()
            .filter_map(|e| match &e.event {
                SessionEvent::TurnFinished { ends_turn, .. } => Some(*ends_turn),
                _ => None,
            })
            .collect()
    }

    // The ordinary sink: published as it is emitted, and it is an ending.
    let hub = Hub::new("s");
    let mut plain = CapturingSink::new(hub.clone());
    plain.emit(ending());
    assert_eq!(
        endings(&hub),
        vec![true],
        "forwarded, and it is the turn's end"
    );

    // The loop's sink: held until the loop says, and said from the decision.
    let mut held = CapturingSink::holding_the_rounds_end(hub.clone());
    held.emit(ending());
    assert_eq!(endings(&hub), vec![true], "held: nothing published yet");
    held.finish(true);
    assert_eq!(endings(&hub), vec![true, false], "another round follows");
    held.finish(false);
    assert_eq!(endings(&hub), vec![true, false], "said once, not twice");
}

/// The question the operator pasted and their answer to it: the TUI draws the question row as ONE line and names it on
/// the answer's row. This writes the two rows' texts as the session writes them and pins them byte for byte; re-bless
/// with `LETIBOT_BLESS_ASKED_CARD=1` and teach `asked_card` the new shape.
#[test]
fn the_tuis_asked_card_fixtures_are_what_the_session_writes() {
    let asked = crate::answers::Asked {
        question: letibot_tools::builtins::intent::Question {
            text: "Row 4: DuckDB's generated `slt_good_*` family is 605 files under test/random \
                   and test/index, and 1,174,513 of the 1,189,434 measured failures — but the \
                   gate currently never enumerates those directories at all, so nothing judges \
                   them. Whatever the row decides, the measured number moves by an order of \
                   magnitude.\n\nA — treat them as in scope and put a DuckDB dialect frontend in \
                   front of the engine, so the generated files run as DuckDB means them; the \
                   count then measures the engine.\n\nB — exclude the family, with the reason \
                   written down in the gate, so the number is the hand-written tests only.\n\nC \
                   — treat them as in scope with no dialect layer, and count every dialect \
                   mismatch as an engine failure; the number is honest and mostly noise."
                .into(),
            options: vec![
                "A — in scope, via a DuckDB dialect frontend".into(),
                "B — exclude it, reason written down".into(),
                "C — in scope, no dialect layer: count it all as engine failures".into(),
                "Not now — leave the row open and the directories unenumerated".into(),
            ],
            because: "Row 4 cannot be resolved by me without over-committing the project to \
                      1.17 million failures or excluding them on my own judgement"
                .into(),
            ..Default::default()
        },
        answer: QuestionAnswer::choosing(0),
        by: "dead".into(),
    };
    let said = operator_answer_text(&asked).expect("an answer");
    let dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../tui/src/app/tests/fixtures");
    for (name, text) in [
        ("asked_question_row.txt", question_row_text(&asked)),
        ("asked_answer_row.txt", said),
    ] {
        let path = dir.join(name);
        if std::env::var_os("LETIBOT_BLESS_ASKED_CARD").is_some() {
            std::fs::write(&path, &text).unwrap();
        }
        let pinned =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert_eq!(
            text,
            pinned,
            "the session no longer writes what the TUI's asked card parses ({})",
            path.display()
        );
    }
}
