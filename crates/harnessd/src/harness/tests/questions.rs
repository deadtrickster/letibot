//! Questions a session asks and the rows its answers become.

use super::*;

/// **The answer row carries what the person said, and nothing of ours.**
///
/// The rule the `!` line's own row keeps — *their words, verbatim* — one vocabulary over.
/// A label, an index or a harness sentence glued to the front would be a citable utterance
/// of the wrong thing, and the trail cites this row.
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

/// **The two rows: the question is the session's, the answer is the operator's.**
///
/// The speakers are the whole of what an `ALLOW <n>` rests on, so they are asserted on the
/// rows the session writes and not on a rendering.
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
            && asked_text.contains("postgres | sqlite"),
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

/// **A job this daemon never ran is answered from the record, not refused.**
///
/// The operator, after a restart: entering a job showed nothing at all — `no job ... here`
/// about a job whose ending was written down. Three claims, and the third is the one a
/// reader acts on: the state word is the row's own, the bytes are admitted LOST rather than
/// silently absent, and a redirect names the file that still holds them (R41).
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

/// **A row announced through a capturing sink is filled by the pairing rule, and
/// body-less without it** — the compaction defect at the smallest size there is.
///
/// This is the free half of `Harness::reconcile` (`reconcile_rows`), driven the way
/// the summary-turn helper drives it. The false-first direction is the assertion that
/// matters: a sink announces on `emit` and carries the id, so a caller that drops the
/// sink has announced a row it never filled — which is what the daemon's view then
/// serves to every head and to every `/resync`.
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
