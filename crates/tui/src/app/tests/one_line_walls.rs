//! **The walls the review measured on real rows, one notice at a time.** A notice drawn at eighty
//! columns is one screen row or two, and the whole of it is a click away. The texts here are the
//! producers' own: each wall was a stored row, so a fixture that is shorter than one proves nothing.

use super::redcard::{holds, popup_text, with_notice};
use super::*;

use crate::ui::panes::notes::{note_card, note_lines};
use crate::ui::transcript::item::system_row_lines;
use crate::ui::transcript::user::session_row_lines;

/// A clock, so a row pays for its timestamp the columns a real one does.
const TS: u64 = 1_760_000_000_000;

fn cfg80() -> RenderConfig {
    RenderConfig {
        width: 80,
        color: false,
        ..RenderConfig::default()
    }
}

/// One session row, drawn through the transcript's own renderer at eighty columns: its screen
/// rows, the glass, and the text its popup holds.
fn drawn(text: &str) -> (usize, String, String) {
    let rows = session_row_lines(text, TS, &cfg80(), &[], &std::collections::HashMap::new()).len();
    let mut a = with_notice(text);
    let glass = a.screen(80, 40).join("\n");
    (rows, glass, newest_notice(&mut a))
}

/// The rule this file holds every notice to.
fn assert_one_line(text: &str) {
    let (rows, glass, popup) = drawn(text);
    assert!(
        (1..=2).contains(&rows),
        "the notice took {rows} screen rows at 80 columns:\n{glass}"
    );
    assert_eq!(popup, text, "the popup is the row whole");
}

/// One System row, drawn at eighty columns: its screen rows and the text its popup holds.
fn drawn_system(text: &str, origin: letibot_transcript::SystemOrigin) -> (usize, String) {
    let rows = system_row_lines(text, origin, TS, &cfg80()).len();
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Event(env(
        1,
        testing::appended("s#t2.0", "system"),
    )));
    a.record_item(
        "s#t2.0",
        TranscriptItem::System {
            text: text.into(),
            origin,
        },
    );
    a.visibility = Visibility::of(Profile::LOUD);
    a.invalidate_history_from(0);
    let glass = a.screen(80, 40).join("\n");
    assert!(rows > 0, "the row drew nothing:\n{glass}");
    (rows, newest_notice(&mut a))
}

/// The text a note's one line opens, and the rows it takes at eighty columns.
fn drawn_note(n: &Note) -> (usize, String) {
    let rows = note_lines(&cfg80(), n).len();
    let (card, text, _) = note_card(n).unwrap_or_else(|| panic!("no card for {n:?}"));
    assert!(!card.line.is_empty());
    (rows, text)
}

/// A head holding one note at the end of its conversation, as the frame would have it.
fn with_note(n: Note) -> App {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.note(n);
    a
}

/// A note's card, clicked, and the popup it opens.
fn click_note(n: Note) -> (String, String) {
    let mut a = with_note(n);
    let screen = a.screen(80, 30);
    let (y, _) = *a
        .card_rows
        .first()
        .unwrap_or_else(|| panic!("the note has no click target: {}", screen.join("\n")));
    a.key(Key::Click { x: 4, y: y as u16 });
    (popup_text(&mut a), screen.join("\n"))
}

fn warned(code: &str, detail: &str) -> Note {
    Note::Warned(Warned {
        code: code.into(),
        detail: detail.into(),
        ts: 1,
    })
}

/// A tool result that hit the output cap, once a call: thousands of sentences for one fact.
#[test]
fn a_length_cap_salvage_is_one_row_at_eighty_columns() {
    let mut text = String::from(
        "Tool call read was not executed: the response hit the output token limit, so its \
         arguments may be truncated. Re-issue the tool call with complete arguments.",
    );
    for _ in 0..6901 {
        text.push_str(
            "\nTool call bash was not executed: the response hit the output token limit, so its \
             arguments may be truncated. Re-issue the tool call with complete arguments.",
        );
    }
    let (rows, glass, popup) = drawn(&text);
    assert_eq!(rows, 1, "{}", glass);
    assert!(
        glass.contains("length cap · 6902 calls not executed"),
        "{glass}"
    );
    assert!(
        !glass.contains("Re-issue"),
        "the wall is on the glass:\n{glass}"
    );
    assert!(holds(
        &popup,
        "Re-issue the tool call with complete arguments."
    ));
}

/// A parent's brief to its child is prose, and it is not a review: only review-shaped ones were
/// folded, so every other one was drawn as it arrived.
#[test]
fn a_parents_message_is_one_row_at_eighty_columns() {
    let text = "message from your parent session `s-gatekeeper`: Two new constraints from the \
                operator, fold them into the plan before you finish. First, the retry budget is \
                three attempts per endpoint and the fourth refusal is final. Second, the notes go \
                in a commit of their own. Say in your reply which rows carry them.";
    let (rows, glass, popup) = drawn(text);
    assert_eq!(rows, 1, "{glass}");
    assert!(
        glass.contains("parent s-gatekeeper · Two new constraints"),
        "{glass}"
    );
    assert_eq!(popup, text);
}

/// The intent check's own findings list: the head of it says the counts and the board, and the
/// rest is the popup's.
#[test]
fn an_intent_check_is_one_row_at_eighty_columns() {
    let text = "[intent check] this turn declared 6 item(s); 0 tool call(s) succeeded and 0 ran \
                without producing a result.\n  - #1 \"add the retry budget\" was declared this \
                turn and no tool ran in this turn — start it, or say why not\n  - #2 \"run the \
                suite\" was declared this turn and no tool ran in this turn — start it, or say \
                why not\n  - #3 \"write the notes\" has been in progress since turn 4 with \
                nothing measured against it\n  … and 3 more; call `todo` with `op: \"list\"` for \
                the rest\nboard: 0 of 6 complete — 0 in progress";
    let (rows, glass, popup) = drawn(text);
    assert_eq!(rows, 1, "{glass}");
    assert!(
        glass.contains("intent check · 6 declared, 0 ran"),
        "{glass}"
    );
    assert!(!glass.contains("3 more"), "{glass}");
    assert_eq!(popup, text);
}

/// A context check the model is given, and the older shape stored transcripts still hold.
#[test]
fn a_context_check_is_one_row_at_eighty_columns() {
    for text in [
        "[context check] Don't size the work by context; the harness handles it. If context was \
         the only reason you stopped, continue the task.",
        "[context check] you said \"my context is nearly out\". Your context is 54% full — \
         547,198 of 999,999 tokens. That is not the wall, and the harness handles it. Continue \
         the task.",
    ] {
        assert_one_line(text);
    }
}

/// A project with no gate: the queue's sentence about the missing section, which is the one the
/// queue writes to a session rather than to a head.
#[test]
fn a_queue_with_no_gate_is_one_row_at_eighty_columns() {
    let text = "The merge queue cannot land anything in `/home/dead/Projects/firecode`: its \
                target `main` has no `Merge gate` section in AGENTS.md, so there is nothing to \
                judge a branch by (`agent/retry-budget` is waiting there). Call `merge_gate` with \
                repo `/home/dead/Projects/firecode` to see what it suggests and ask the operator \
                with `ask_user_question` (options = those choices; they may type their own \
                command). Exception to the queue rule: once they pick, add the section to that \
                repository's AGENTS.md and commit it to `main` yourself. The queue goes on by \
                itself when the section is there.";
    let (rows, glass, popup) = drawn(text);
    assert_eq!(rows, 1, "{glass}");
    assert!(glass.contains("cannot land anything in"), "{glass}");
    assert!(!glass.contains("queue goes on by itself"), "{glass}");
    assert_eq!(popup, text);
}

/// What a re-seat told the model, written into the conversation as a row: the heading, the
/// summarised half, and the three tool lists.
#[test]
fn a_reseat_is_one_row_at_eighty_columns() {
    let text = "[reseat] Your system prompt and tools were updated; earlier history is summarised \
                above. New: task_resume. Removed: job_wait. Changed (read before calling): \
                todo_write. Carry on.";
    let (rows, glass, popup) = drawn(text);
    assert_eq!(rows, 1, "{glass}");
    assert!(glass.contains("reseated · summarised"), "{glass}");
    assert!(!glass.contains("Carry on"), "{glass}");
    assert_eq!(popup, text);
}

/// A wake carrying a job's completion and a monitor's firing: one fold had no monitor arm, so the
/// WHOLE row fell to raw and both walls were back.
#[test]
fn a_monitor_beside_a_job_keeps_both_compact() {
    let text = "[job] a job you backgrounded has ended:\n  - `build · j7` exited 0 after 4.4s, \
                wrote 0 bytes: cargo build\nOutput: `job_output` job=\"j7\".\n\n[monitor] 1 \
                watch(es) fired:\n  - `ci` (command `gh run watch` exiting), declared by turn \
                s#t8#226: FIRED after 0.3s — the command exited 2\nOnly a FIRED watch is an \
                answer about the world.";
    let (rows, glass, popup) = drawn(text);
    assert_eq!(
        rows, 3,
        "the job's own card is two lines a job and the monitor one:\n{glass}"
    );
    assert!(glass.contains("j7 · build · exited 0 4.4s"), "{glass}");
    assert!(glass.contains("monitor · ci FIRED after 0.3s"), "{glass}");
    assert!(!glass.contains("Only a FIRED watch"), "{glass}");
    assert_eq!(popup, text);
}

/// The progress watcher's mid-run correction, with the two paragraphs after it.
#[test]
fn the_progress_watcher_is_one_row_at_eighty_columns() {
    let text = "The harness is watching what your tool calls return, not how many you make, and \
                the last 4 rounds ran 8 calls and 0 of 8 returned a result this turn had not \
                already seen (8 ok). No call was repeated, so the questions were new and nothing \
                could answer them.\n\nIf you already have what you need, answer now. If you do \
                not, ask a *different* question — the same one returns the same bytes. If the \
                next round also produces nothing new the turn stops and the operator is shown \
                this, so say in that round what you are still looking for.";
    let (rows, glass, popup) = drawn(text);
    assert_eq!(rows, 1, "{glass}");
    assert!(
        glass.contains("progress · last 4 rounds: 8 calls, 0"),
        "{glass}"
    );
    assert!(!glass.contains("ask a *different* question"), "{glass}");
    assert_eq!(popup, text);
}

/// A child's turn ending with its worktree dirty: one nudge, and the file list and the two
/// commands are the popup's.
#[test]
fn the_deliverable_nudge_is_one_row_at_eighty_columns() {
    let text = "[the deliverable] Your worktree `/home/dead/Projects/letibot/wt-r89t` has \
                uncommitted changes, so the branch `agent/one-line-walls` does not hold them: M \
                crates/tui/src/app/tests/mod.rs, ?? crates/tui/src/app/tests/one_line_walls.rs. \
                The branch is the deliverable — commit this work to `agent/one-line-walls` now \
                (or say, for any file that must stay out, why), check that `git status \
                --porcelain` prints nothing, and then finish, naming the commit(s) in your \
                answer.";
    let (rows, glass, popup) = drawn(text);
    assert_eq!(rows, 1, "{glass}");
    assert!(
        glass.contains("deliverable · agent/one-line-walls"),
        "{glass}"
    );
    assert!(!glass.contains("git status"), "{glass}");
    assert_eq!(popup, text);
}

/// The stale prompt, written into the conversation as well as raised as a warning: the sentence
/// is long and the row must still be one line, saying which of the three tool lists moved.
#[test]
fn a_stale_prompt_session_row_is_one_row_at_eighty_columns() {
    let text = "session s-gatekeeper was seated 10-10 22:42 with a prompt this daemon would not \
                compose now: the tool schemas have changed since. This session keeps the prefix \
                it was created with — message 0 is frozen, and rewriting it is a full cold \
                re-prefill — so the model cannot call a tool seated since, and a schema that \
                grew is one it has never been told about. Seated here but not in this \
                conversation's prompt, so the model cannot call them: task_resume. In the prompt \
                but not seated here, so a call to them refuses: job_wait. Seated on both sides \
                with a different schema, so the model is working from the older one: \
                todo_write. `/reseat` rebuilds the prompt from what is seated now, carrying this \
                conversation across as it is; a new session gets the seated one from the start.";
    let (rows, glass, popup) = drawn(text);
    assert_eq!(rows, 1, "{glass}");
    assert!(
        glass.contains("prompt stale · seated 10-10 22:42"),
        "{glass}"
    );
    assert!(!glass.contains("/reseat rebuilds"), "{glass}");
    assert_eq!(popup, text);
}

/// The operator's answer to one of the plan's own questions, as the row keeps it.
#[test]
fn an_operators_answer_is_one_row_at_eighty_columns() {
    let text = "[answer] on \"GAP (latency)\": you asked \"Postpone the latency row to the next \
                release?\" — the operator chose \"Postpone it\". The row no longer waits on the \
                operator.";
    let (rows, glass, popup) = drawn(text);
    assert_eq!(rows, 1, "{glass}");
    assert!(glass.contains("answer · chose \"Postpone it\""), "{glass}");
    assert_eq!(popup, text);
}

/// A child's completion handed up to a session because the one that started it has stopped: the
/// closing is a different sentence and the fold did not know it.
#[test]
fn an_orphan_childs_completion_is_one_row_at_eighty_columns() {
    let text = "[task] a subagent a session below this one started has finished:\n  - \
                `s-9-sub-1` done: all green (started by `s-9`)\nThis session is told because the \
                one that started it cannot be: it has stopped and will not run again. The handle \
                is in this tree, so the whole answer is still there: `task_result` \
                task=\"s-9-sub-1\".";
    let (rows, glass, popup) = drawn(text);
    assert_eq!(rows, 1, "{glass}");
    assert!(
        glass.contains("Agent s-9-sub-1 · done: all green"),
        "{glass}"
    );
    assert!(!glass.contains("This session is told"), "{glass}");
    assert_eq!(popup, text);
}

/// The two nudges the turn tail writes: one for a turn stopped at the wall, and the one that says
/// a turn spent its output on reasoning.
#[test]
fn the_wall_nudges_are_one_row_at_eighty_columns() {
    let wall = "Your previous turn was stopped at the context wall and the conversation was then \
                compacted: the summary above stands in for everything said before it. Continue \
                the work you were doing, picking up from the summary rather than repeating what \
                it already records.";
    let (rows, glass, popup) = drawn(wall);
    assert_eq!(rows, 1, "{glass}");
    assert!(
        glass.contains("context wall · compacted, continuing from"),
        "{glass}"
    );
    assert_eq!(popup, wall);
    let over = "You keep overthinking. Cut it short and do things. Your last turn spent its whole \
                output on reasoning (reasoning_only) and nothing was recorded. Answer first. \
                Reason after, or not at all.";
    let (rows, glass, popup) = drawn(over);
    assert_eq!(rows, 1, "{glass}");
    assert!(
        glass.contains("nudge · reasoning-only turn, told to answ"),
        "{glass}"
    );
    assert_eq!(popup, over);
}

/// A System row: the compaction's summary, the re-seat's note and the summary turn's own
/// instruction. Drawn whole, one of these was a hundred and ninety screen rows.
#[test]
fn a_system_row_is_one_row_at_eighty_columns() {
    use letibot_transcript::SystemOrigin::Update;
    let summary = format!(
        "This conversation was compacted: what was said before this point is replaced by the \
         summary below, which was written over the full history of transcript s#t24 and proposed \
         no tool calls.\n\n## Objective\n\n- {}\n",
        "THE RECORD OF TWENTY THOUSAND CHARACTERS ".repeat(400)
    );
    for text in [
        summary,
        "System prompt and tools updated; the conversation continues unchanged. Nothing was \
         summarised."
            .to_string(),
        "The conversation above is what this record stands in for: it is what a reader will have \
         instead of it, and anything you do not carry into it is lost. Write the record in \
         exactly these sections, in this order, and keep every one of them even when it is \
         empty:\n## Objective\n## Important Details\n## Work State\n### Completed\n### Active\n\
         ### Blocked\n## Next Move\n## Relevant Files\nRules: terse bullets, not prose \
         paragraphs. Do not call tools."
            .to_string(),
    ] {
        let (rows, popup) = drawn_system(&text, Update);
        assert_eq!(rows, 1, "the row took {rows} screen rows: {text:.80}");
        assert_eq!(popup, text, "the popup is the row whole");
    }
}

/// Warning codes no card reads: each is one clickable line in its register, and the whole detail
/// is one click away.
#[test]
fn a_warning_no_card_reads_is_one_clickable_row_at_eighty_columns() {
    for (code, detail) in [
        (
            "resume_note",
            "resumed: this session was carried across a daemon restart. 7 items, the ledger at \
             412 entries, and the tool list rebuilt from what is seated now.",
        ),
        (
            "merge_queue_store_error",
            "the queue's store could not be read: database is locked, so nothing was reviewed \
             this pass and the queue is not moving. It tries again on its next poll.",
        ),
        (
            "queue_lease_lost",
            "this daemon's lease on `/home/dead/Projects/firecode` was taken by dead@box-2 at 41 \
             seconds old; it stops reviewing and landing until it takes the lease back.",
        ),
        (
            "waiting_on_operator",
            "the model asked \"which database?\" on the row `deploy` and nothing has answered \
             it: the plan goes on, that row does not.",
        ),
    ] {
        let (rows, text) = drawn_note(&warned(code, detail));
        assert_eq!(rows, 1, "{code} took {rows} rows");
        assert_eq!(text, detail, "{code}");
        let (popup, glass) = click_note(warned(code, detail));
        assert!(holds(&popup, detail), "{code}: {popup}");
        assert!(!holds(&glass, "It tries again"), "{code}: {glass}");
    }
}

/// The two codes the head's own census has to name: a code it has never been told about is drawn
/// as a failure, so a code that IS a failure still has to be a row somebody wrote down.
#[test]
fn a_warning_the_census_names_is_one_row_at_eighty_columns() {
    use letibot_sessionlog::warning::{Class, class, family};
    assert_eq!(family("denied:req-7"), "denied");
    assert_eq!(class("denied:req-7"), Class::Failure);
    assert_eq!(class("memory_high"), Class::Failure);
    for code in ["denied:req-7", "memory_high"] {
        assert!(
            letibot_sessionlog::warning::TABLE
                .iter()
                .any(|(c, _)| *c == family(code)),
            "{code} is not in the census"
        );
    }
    for (code, detail) in [
        (
            "denied:req-7",
            "the operator refused `bash` running `rm -rf target`: denied once, at the head. The \
             call did not happen and only the operator can lift it.",
        ),
        (
            "memory_high",
            "the daemon's resident memory crossed the second step: 3.1 GiB of a 4.0 GiB cap, \
             with 5 sessions holding most of it. A daemon the kernel kills takes every session \
             with it.",
        ),
    ] {
        let (rows, text) = drawn_note(&warned(code, detail));
        assert_eq!(rows, 1, "{code} took {rows} rows");
        let (popup, _) = click_note(warned(code, detail));
        assert!(holds(&popup, &text), "{code}: {popup}");
    }
}

/// A note this head made for itself: a refusal nobody made, an answer, a settled decision and the
/// warnings the head raises about its own plumbing. Each is one line, its text a click away.
#[test]
fn a_head_made_note_is_one_row_at_eighty_columns() {
    let long = "the call could not be read: its arguments were not JSON, so nothing was run and \
                nothing was refused. The model's next attempt is where this is fixed.";
    for n in [
        Note::NotRun(Warned {
            code: "not_run".into(),
            detail: long.into(),
            ts: 1,
        }),
        Note::Answered {
            req_id: "q1".into(),
            summary: "deploy to staging".into(),
            by: "human dead".into(),
            said: "not before the gate is green, and the gate has not run since the last push \
                   onto the branch"
                .into(),
        },
        warned(
            "protocol_skew",
            "the daemon speaks protocol 7 and this head speaks 6: frames either side may be \
             misread, and the session's rows are only as good as the half that agrees.",
        ),
        warned(
            "anchor_lost",
            "the row this viewport was holding is gone, because a compaction replaced the \
             transcript around it. The reader was moved to the top of what is left.",
        ),
        warned(
            "sudo",
            "the password prompt settled with nobody giving one: `sudo` wanted a password and \
             this head had no terminal to ask on, so the command did not run.",
        ),
        warned(
            "prompt",
            "nothing sent: the program waiting on the answer had gone by the time the words \
             arrived, so the line you typed reached nothing at all.",
        ),
        warned(
            "provider_key_refused",
            "the provider refused the key: 401 on the first request of the turn, so nothing was \
             asked and the model saw no prompt.",
        ),
        warned(
            "daemon_replaced",
            "this head outlived its daemon: the registry is in memory, so the answers still on \
             the screen were a predecessor's.",
        ),
        warned(
            "unreadable_frame",
            "a frame from the daemon could not be read: field `items` was missing, so the rows \
             after it are not on this screen.",
        ),
    ] {
        let (rows, text) = drawn_note(&n);
        assert_eq!(rows, 1, "{n:?} took {rows} rows");
        assert!(!text.is_empty());
        let (popup, _) = click_note(n.clone());
        assert!(holds(&popup, &text), "{n:?}: {popup}");
    }
}

/// A pane's ending is the one note drawn whole, on purpose: the rows a program leaves on the
/// screen as it dies are the useful ones, so folding them away would keep the least of it. Its
/// own line - what ran, and why it ended - is still one row.
#[test]
fn a_panes_ending_names_one_row_and_keeps_what_the_program_said() {
    let n = Note::Pane {
        line: "!term cargo test --workspace --no-fail-fast --offline".into(),
        said: vec![
            "running 1233 tests".into(),
            "test app::tests::one_line_walls ... ok".into(),
            "failures: app::tests::queue::a_row".into(),
            "error: test failed, to rerun pass `-p letibot-tui --lib`".into(),
        ],
        reason: "exited 101".into(),
        closed: false,
    };
    assert!(
        note_card(&n).is_none(),
        "a pane's ending opens nothing: it IS the text"
    );
    let rows = note_lines(&cfg80(), &n);
    assert_eq!(
        rows.len(),
        5,
        "one row naming it and four of the program's: {rows:?}"
    );
    assert!(
        rows[0].contains("!term cargo test") && rows[0].contains("exited 101"),
        "{:?}",
        rows[0]
    );
    assert!(rows[4].contains("error: test failed"), "{:?}", rows[4]);
}

/// **A settled decision leads with its verdict.** The card led with the summary, so at eighty
/// columns the verdict began past the cut and an allow and a refuse drew identically in monochrome;
/// and a summary carrying a newline put the verdict on a line the card never read.
#[test]
fn a_settled_decision_leads_with_its_verdict() {
    use letibot_sessionlog::event::{Decider, DecisionOutcome};
    let decided = |summary: &str| {
        Note::Decided(Box::new(SettledDecision {
            req_id: "r1".into(),
            kind: String::new(),
            call_id: None,
            summary: summary.into(),
            outcome: DecisionOutcome::Selected {
                option_id: "deny".into(),
            },
            by: Decider {
                kind: "human".into(),
                identity: "dead".into(),
            },
            basis: "dead chose `deny` at the head; the registry token was not in place".into(),
            advice: None,
            late: false,
        }))
    };
    for summary in [
        "run `cargo publish --dry-run` on the branch",
        // A multi-line command is put in the summary verbatim, so the verdict lands on the last
        // line of it - the one the card never read.
        "run this and wait:\nbash -lc 'git push --force'\nthen say what happened",
    ] {
        let n = decided(summary);
        let rows = note_lines(&cfg80(), &n);
        assert!(
            rows[0].contains("REFUSED (deny)"),
            "the verdict is not on the row at eighty columns: {:?}",
            rows[0]
        );
        let (popup, _) = click_note(n);
        assert!(
            holds(&popup, "REFUSED (deny)"),
            "the popup lost the verdict: {popup}"
        );
    }
}

/// A decision that settled with no call to hang it on is a note of its own, and it was the one
/// head-made note with no card: three wrapped rows of a permission's outcome.
#[test]
fn a_settled_decision_is_one_row_at_eighty_columns() {
    use letibot_sessionlog::event::{Decider, DecisionOutcome};
    let n = Note::Decided(Box::new(SettledDecision {
        req_id: "r1".into(),
        kind: String::new(),
        call_id: None,
        summary: "run `cargo publish --dry-run` on the branch".into(),
        outcome: DecisionOutcome::Selected {
            option_id: "deny".into(),
        },
        by: Decider {
            kind: "human".into(),
            identity: "dead".into(),
        },
        basis: "dead chose `deny` at the head; the registry token was not in place".into(),
        advice: None,
        late: false,
    }));
    let (rows, glass) = {
        let mut a = with_note(n.clone());
        let glass = a.screen(80, 30).join("\n");
        (note_lines(&cfg80(), &n).len(), glass)
    };
    assert!(
        (1..=2).contains(&rows),
        "the note took {rows} rows:\n{glass}"
    );
    let (popup, _) = click_note(n.clone());
    assert!(holds(&popup, "run `cargo publish --dry-run`"), "{popup}");
    assert!(
        holds(&popup, "the registry token was not in place"),
        "{popup}"
    );
    // A refusal has to look refused: the word leads the row and is in the popup, and the line is
    // drawn in the failure register. See `settled`.
    assert!(holds(&popup, "REFUSED (deny)"), "{popup}");
    let (card, _, _) = note_card(&n).expect("a card");
    assert_eq!(card.role, Role::Failure);
}

/// A compaction's note: it said its code twice and `· /notes` was a seam nothing could click.
#[test]
fn a_compaction_note_is_one_clickable_row_at_eighty_columns() {
    let detail = "compacted: 958397 → 100005 tokens, on transcript s#t25. Nothing of the summary \
                  turn reached this screen — it ran over a scratch transcript — so here is what \
                  the model now reads in place of everything before it:\n\n## Goal\n\nTHE \
                  MODEL'S OWN RECORD";
    let (rows, text) = drawn_note(&warned("compacted", detail));
    assert_eq!(rows, 1);
    assert_eq!(text, detail);
    let mut a = with_note(warned("compacted", detail));
    let screen = a.screen(80, 30).join("\n");
    assert_eq!(
        screen.matches("compacted").count(),
        1,
        "the code said once on the row:\n{screen}"
    );
    assert!(!screen.contains("/notes"), "{screen}");
    let (popup, _) = click_note(warned("compacted", detail));
    assert!(holds(&popup, "THE MODEL'S OWN RECORD"), "{popup}");
}

/// A folded settlement row opens the notice it stands for: eight finished children were eight
/// lines with no way to the notice behind them, and a due plan row the same.
#[test]
fn a_folded_settlement_opens_its_notice() {
    let task = "[task] a subagent you started has finished:\n  - `fix-login` done: all green, the \
                branch is `agent/fix-login`\nFull answer: `task_result` task=\"s-1-sub-2\".";
    let (rows, glass, popup) = drawn(task);
    assert_eq!(rows, 1, "{glass}");
    assert!(
        glass.contains("Agent fix-login · done: all green"),
        "{glass}"
    );
    assert_eq!(popup, task);
    let todo = "[todo] Due (its condition is met):\n  - ship the release notes once the build \
                job has ended — `j121` exited 0";
    let (rows, glass, popup) = drawn(todo);
    assert_eq!(rows, 1, "{glass}");
    let mut a = with_notice(todo);
    let screen = a.screen(80, 30);
    let (y, _) = *a
        .notice_rows
        .first()
        .unwrap_or_else(|| panic!("the row is not clickable: {}", screen.join("\n")));
    a.key(Key::Click { x: 4, y: y as u16 });
    assert!(
        holds(&popup_text(&mut a), "ship the release notes"),
        "{glass}"
    );
    assert_eq!(popup, todo);
}
