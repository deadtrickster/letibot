//! The composer: typing, completion, history, sending.

use super::*;

/// **A bare line is not spent on the model while the operator's own command waits.**
///
/// The measured case, 2026-10-09: `! sudo apt install mc`, the password taken, `apt` at
/// `Continue? [Y/n]` under root where `/proc` refuses — so no card — and the person typed
/// `y` here. The line became a prompt and reached the model; the command they had typed
/// waited unanswered until its deadline killed it. While the daemon's request for their run
/// is still open — the card up, or put away with esc, which does not end the run and does
/// not close the request — a bare line must not quietly become the model's: it is held, and
/// the sentence names the two doors (`!send` for the command, and waiting for the run to
/// end for the model).
#[test]
fn a_bare_line_while_the_runs_request_is_open_is_not_spent_on_the_model() {
    let mut a = app();
    a.session_id = "s".into();
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::PromptRequested {
            req_id: "r1".into(),
            job: "j1".into(),
            command: "sudo apt install mc".into(),
            question: Some("Continue? [Y/n]".into()),
        },
    )));
    // **The card is put away, not answered** — esc's own act. The daemon keeps the request
    // open and the run keeps waiting, which is exactly the window the measured `y` fell
    // through.
    a.key(Key::Esc);

    let spent = a.submit("y".into());
    assert!(
        spent.is_none(),
        "a bare `y` while the operator's own command waits must not become a prompt: {spent:?}"
    );
    assert_eq!(a.input(), "y", "the words are held, not swallowed");
    assert!(
        a.pending_prompts.is_empty(),
        "nothing was queued as a prompt either"
    );
}

/// **And the refusal answers nothing, in the head rather than in the matcher.**
///
/// The matcher saying `Ambiguous` is not enough: `submit` is what turns a keypress
/// into an `Action`, and a refusal that still answered the marked row would hand the
/// daemon a grant the operator never chose. So: no action, the words still in the
/// composer, the ask still open, and the candidates on the notice line.
#[test]
fn an_ambiguous_prefix_answers_nothing_and_keeps_the_words() {
    use letibot_sessionlog::event::{OnTimeout, OptionKind};
    let mut a = app();
    a.open.push(decision_with(&[
        OptionKind::AllowOnce,
        OptionKind::AllowSession,
        OptionKind::AllowAlways,
        OptionKind::RejectOnce,
    ]));
    a.open[0].on_timeout = OnTimeout::Deny;
    let pushed = a.open.len();

    assert_eq!(
        a.submit("allow".into()),
        None,
        "an ambiguous name must answer nothing at all"
    );
    // The card is still open, the words are still theirs, and the notice names the
    // three they might have meant — rather than the courtesy line, which would mean
    // the marked row had been answered for them.
    assert_eq!(a.open.len(), pushed, "the ask was answered anyway");
    assert_eq!(a.input(), "allow");
    let said = a.notice.clone().unwrap_or_default();
    for id in ["allow_once", "allow_session", "allow_always"] {
        assert!(said.contains(id), "{said}");
    }
    assert!(
        !said.contains("answered the ask"),
        "the marked row was answered under an ambiguous name: {said}"
    );

    // Finishing the word answers it. The ask stays in `open` until the daemon says
    // it settled — the head does not remove it on the strength of its own submit.
    a.set_composer("allow_session");
    assert_eq!(
        a.submit("allow_session".into()),
        Some(Action::Answer {
            req_id: "d1".into(),
            option_id: "allow_session".into(),
            pattern: None,
            note: None
        })
    );
}

/// **A `!` line's word completes as a file name**, the way a shell's does — the
/// operator's `! ./stroppy/build/stroppy`, typed out whole because nothing offered it.
#[test]
fn a_bang_lines_word_completes_against_the_workspace() {
    let d = path_fixture();
    let ws = d.to_str().unwrap();
    let line = |t: &str| complete_path_word(t, ws);
    // A path in command position: one directory, then the next, then the file.
    assert_eq!(
        line("! ./st"),
        Some(PathCompletion::Line("! ./stroppy/".into()))
    );
    assert_eq!(
        line("! ./stroppy/build/st"),
        Some(PathCompletion::Line("! ./stroppy/build/stroppy ".into()))
    );
    // An argument completes as a file; two matches extend to what they share.
    match line("! cat REA") {
        Some(PathCompletion::Choices { line, names }) => {
            assert_eq!(line, "! cat README.");
            assert_eq!(names, vec!["README.md".to_string(), "README.old".into()]);
        }
        other => panic!("{other:?}"),
    }
    // A space in a name is escaped, as a shell would need it.
    assert_eq!(
        line("! cat my"),
        Some(PathCompletion::Line("! cat my\\ file.txt ".into()))
    );
    // Hidden names only when asked for.
    assert_eq!(
        line("! cat .hi"),
        Some(PathCompletion::Line("! cat .hidden ".into()))
    );
    assert!(
        matches!(line("! ls "), Some(PathCompletion::Choices { names, .. }) if !names.iter().any(|n| n.starts_with('.')))
    );
    // The command word, spelled as a word, is the history's to complete, not a file's.
    assert_eq!(line("! REA"), None);
    // Nothing matches, a quote or a `$` in the word: left to the line completions.
    assert_eq!(line("! cat zzz"), None);
    assert_eq!(line("! cat \"REA"), None);
    assert_eq!(line("! cat $HO"), None);
    let _ = std::fs::remove_dir_all(&d);
}

/// The same, through the keys: Tab completes the composer, and the choices are drawn
/// in the completion row while the line is unchanged.
#[test]
fn tab_on_a_bang_line_completes_the_path_and_shows_the_choices() {
    let d = path_fixture();
    let mut a = app();
    a.wiring.workspace = d.to_str().unwrap().into();
    typed(&mut a, "! cat ./stro");
    a.key(Key::Tab);
    assert_eq!(a.input(), "! cat ./stroppy/");
    a.key(Key::Tab);
    assert_eq!(a.input(), "! cat ./stroppy/build/");
    let mut b = app();
    b.wiring.workspace = d.to_str().unwrap().into();
    typed(&mut b, "! cat REA");
    b.key(Key::Tab);
    assert_eq!(b.input(), "! cat README.");
    let screen = b.screen(100, 20).join("\n");
    assert!(
        screen.contains("README.md") && screen.contains("README.old"),
        "{screen}"
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn up_recalls_the_queued_line_for_editing_and_takes_it_back() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    typed(&mut a, "also fix the parser");
    a.key(Key::Enter);
    typed(&mut a, "and add a test for it");
    a.key(Key::Enter);
    assert_eq!(a.pending_prompts.len(), 1);
    // Up with an empty composer: the queue comes back, the take-back rides.
    assert_eq!(a.key(Key::Up), Some(Action::WithdrawPrompts));
    assert_eq!(a.input(), "also fix the parser\nand add a test for it");
    assert!(a.pending_prompts.is_empty(), "recalled, not held");
    // The operator edits and sends; the edited line queues fresh, and the
    // daemon's take-back means it replaces the original rather than
    // stacking onto it.
    typed(&mut a, " — no, just the parser");
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::Prompt(
            "also fix the parser\nand add a test for it — no, just the parser".into()
        ))
    );
    assert_eq!(a.pending_prompts.len(), 1);
}

/// **R6: a line typed while the conversation is still being imported is held.**
///
/// The daemon answers a prompt sent mid-import against the whole history — the import
/// is one worker job, so a prompt cannot interleave — but the head must not show one as
/// `queued` for a turn nobody has started. So it is refused and the words go back to the
/// field they were typed in: the §4.2 behaviour (`7b9ca62`), reused, because answering
/// against a half-adopted transcript and then appending the rest would put the
/// conversation in the wrong order (R2's rule with the whole history missing).
#[test]
fn a_line_typed_during_an_import_is_held_in_the_composer() {
    let mut a = app();
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::Filling {
            what: "importing an opencode conversation".into(),
            unit: "parts".into(),
            done: 0,
            total: 100,
        },
    )));
    for c in "hold me".chars() {
        a.key(Key::Char(c));
    }
    assert_eq!(
        a.key(Key::Enter),
        None,
        "nothing leaves the head while an import is running"
    );
    assert_eq!(
        a.input(),
        "hold me",
        "the words are still the operator's, in the field they typed them into"
    );
    let s = a.screen(100, 30).join("\n");
    assert!(s.contains("still being imported"), "{s}");
}

/// **The held `!` line is recallable, and the take-back rides with it.**
///
/// `↑` on an empty composer pulls the queued line back for editing and asks the
/// daemon to drop it — and for a `!` line that is not a courtesy but the whole
/// point: a command that ran after it was visibly taken back would be work nobody
/// re-asked for. (The daemon's half — dropping the queued `OperatorShell` with the
/// prompts — is pinned in `letibot-sessionlog`'s `operator_shell.rs`.)
#[test]
fn a_held_bang_line_is_recalled_by_up_and_the_take_back_rides_with_it() {
    let mut a = app();
    typed(&mut a, "! make -j8");
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::OperatorShell {
            line: "! make -j8".into()
        })
    );
    assert_eq!(a.key(Key::Up), Some(Action::WithdrawPrompts));
    assert_eq!(a.input(), "! make -j8", "the line is back for editing");
    assert!(a.pending_prompts.is_empty(), "the echo stood down");
    // And the edited resend is a `!` line again, not prose that happens to have
    // lost the bang it would need.
    a.key(Key::End);
    typed(&mut a, " && echo done");
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::OperatorShell {
            line: "! make -j8 && echo done".into()
        })
    );
}

/// **`!!` is a command, not a repeat.** There is no history in this composer to
/// repeat from, and inventing that reading would make the same bytes mean two
/// things depending on state nobody can see — so `!!` carries the command `!`,
/// which the shell will answer for itself, and the row that comes back is the
/// honest one. `!x` is pinned beside it: a bang does not need a space.
#[test]
fn a_double_bang_is_a_command_not_a_repeat() {
    let mut a = app();
    typed(&mut a, "!!");
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::OperatorShell { line: "!!".into() })
    );
    assert_eq!(
        letibot_sessionlog::operator_shell_command("!!"),
        Some("!"),
        "the command the daemon will run is `!`, and nothing else"
    );
}

/// **A verb the head will refuse is not offered** — R29 pointed the other way, and the
/// reason the joined list is built from the two OWNERS rather than from anything a
/// person could type (`/bash`, `/write`): neither half will act on those, and a
/// completion that suggests one is a remedy that does not work.
#[test]
fn completion_offers_nothing_neither_half_will_act_on() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Settings {
        rows: vec![letibot_sessionlog::protocol::SettingRow {
            key: letibot_sessionlog::protocol::DAEMON_VERBS_KEY.into(),
            value: "gate,job".into(),
            source: "default".into(),
            editable: String::new(),
            choices: Vec::new(),
            tools: Vec::new(),
        }],
    });
    let all: Vec<String> = a.command_names().into_iter().map(|(n, _)| n).collect();
    for invented in ["bash", "write", "sh", "exec"] {
        assert!(
            !all.iter().any(|n| n == invented),
            "`/{invented}` is offered and neither half acts on it"
        );
    }
}

#[test]
fn a_multi_line_prompt_grows_the_field_and_the_caret_follows() {
    let mut a = app();
    typed(&mut a, "first line");
    a.key(Key::SoftEnter);
    typed(&mut a, "second line");
    let screen = a.screen(80, 24);
    let (row, _) = a.cursor().unwrap();
    assert!(screen[row].contains("second line"), "{:?}", screen[row]);
    assert!(
        screen[row - 1].contains("first line"),
        "{:?}",
        screen[row - 1]
    );
    // Enter still sends the whole thing, both lines.
    match a.key(Key::Enter) {
        Some(Action::Prompt(t)) => assert_eq!(t, "first line\nsecond line"),
        other => panic!("{other:?}"),
    }
    // …and Up recalls it.
    a.key(Key::Up);
    assert_eq!(a.input(), "first line\nsecond line");
}

#[test]
fn a_pasted_stack_trace_collapses_and_is_sent_in_full() {
    let mut a = app();
    typed(&mut a, "why does this happen: ");
    let trace: String = (0..40).map(|i| format!("  at frame {i}\n")).collect();
    a.key(Key::Paste(trace));
    assert!(a.input().contains("[Pasted #1"), "{}", a.input());
    // The composer stayed small; the conversation did not scroll away.
    assert!(a.editor.height(a.composer_cols(), 24) <= 2);
    match a.key(Key::Enter) {
        Some(Action::Prompt(t)) => {
            assert!(t.contains("at frame 39"));
            assert!(!t.contains("[Pasted"));
        }
        other => panic!("{other:?}"),
    }
}

/// **§6: the panes and the promote, reachable as verbs.**
///
/// Each had a chord and no word (or, for `/models` and `/resync`, a word and no
/// listing in the completion table). A pane that can only be opened by a chord is
/// unreachable from a pipe and unteachable by `/help`, and a verb nobody lists is a
/// verb nobody finds. The chords stay — they are faster — and every pair here ends
/// in **one function**, so the two spellings cannot drift.
#[test]
fn the_panes_and_the_promote_are_reachable_as_verbs() {
    // `/todos` opens the pane and asks the daemon for the list, exactly as `ctrl-t`.
    let mut a = app();
    assert_eq!(a.submit("/todos".into()), Some(Action::ListTodos));
    assert!(a.todos_pane);
    // …and closes it again, with no read the second time — a read on the way out
    // would be a round trip for a screen that is going away.
    assert_eq!(a.submit("/todos".into()), None);
    assert!(!a.todos_pane);
    // The chord and the verb agree.
    let mut b = app();
    assert_eq!(b.key(Key::CtrlT), Some(Action::ListTodos));
    assert!(b.todos_pane, "the chord opens the same pane");

    // The subagent tree is the same shape: no bootstrap read, because the tree is
    // folded from durable `Subagent` events a snapshot already carries.
    let mut c = app();
    assert_eq!(c.submit("/subagents".into()), None);
    assert!(c.subagents_pane);
    c.submit("/subagents".into());
    assert!(!c.subagents_pane);

    // `/peek ID` reads one subagent, and remembers it is waiting so Esc can cancel.
    let mut d = app();
    d.session_id = "s1".into();
    assert_eq!(
        d.submit("/peek s-abc".into()),
        Some(Action::Peek("s-abc".into()))
    );
    assert_eq!(d.sub_out_pending.as_deref(), Some("s-abc"));
    // A bare `/peek` says how it is used rather than sending an empty id to a
    // daemon that would answer with a refusal nobody asked for.
    let mut e = app();
    e.session_id = "s1".into();
    assert_eq!(e.submit("/peek".into()), None);
    assert!(e.sub_out_pending.is_none());

    // `/resume ID` brings an on-disk session back *and goes there*: the switch rides
    // the `Sessions` reply, which is what `want_new_session` means.
    let mut f = app();
    f.session_id = "s1".into();
    assert_eq!(
        f.submit("/resume s-xyz".into()),
        Some(Action::ResumeSession("s-xyz".into()))
    );
    assert!(f.want_new_session);

    // **`/promote` needs a command actually running**, and says which of the two
    // silences it is in — the distinction the chord was fixed for.
    let mut g = app();
    g.session_id = "s1".into();
    assert_eq!(g.submit("/promote".into()), None, "nothing is running");
    let mut h = app();
    h.session_id = "s1".into();
    h.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    h.apply(ServerFrame::Event(env(
        2,
        testing::proposed_on("t1", "c1", "bash", "\"cargo test\""),
    )));
    // A turn whose state has already gone terminal but whose CALL is still running:
    // the case the guard exists for, because the two come apart.
    h.apply(ServerFrame::Event(env(3, testing::turn_finished("t1"))));
    h.apply(ServerFrame::Event(env(
        4,
        SessionEvent::ToolStarted {
            turn_id: "t1".into(),
            call_id: "c1".into(),
            name: "bash".into(),
            access: Default::default(),
        },
    )));
    // **The premise is the two questions apart, which is what this fixture is for.** The turn's
    // state name says the round is over, and the call it proposed is still executing — so
    // `turn_generating` is false and the head is plainly busy. That gap is why `/promote` could
    // not be gated on the turn, and why the message below it must ask the calls.
    assert!(!h.turn_generating(), "the premise: the round is over");
    assert!(h.turn_busy(), "the premise: and the model is still working");
    assert!(h.running_call().is_some(), "and its command is not");
    assert_eq!(h.submit("/promote".into()), Some(Action::Promote));
    assert_eq!(
        h.key(Key::CtrlO),
        Some(Action::Promote),
        "the chord and the verb are one action"
    );
}

/// **§6: a verb this head has never heard of is the daemon's to refuse.**
///
/// The head kept a twelve-name allowlist of daemon verbs and answered `unknown
/// command /x — try /help` for anything outside it. That is a second copy of the
/// daemon's table, and its failure mode is the head lying about **its own daemon**:
/// a verb added on the other side is unreachable until this head is rebuilt, and the
/// operator is told the command does not exist by the half that does not own the
/// list.
///
/// Forwarding is safe because the daemon answers an unrecognised verb by name
/// (`/{verb} is not a daemon verb; /help lists the head's`), so the question is
/// settled where the answer lives. This asserts both halves of that: the line goes
/// over, and the head does not add a verdict of its own.
#[test]
fn a_verb_the_head_has_never_heard_of_goes_to_the_daemon_unjudged() {
    let mut a = app();
    a.session_id = "s1".into();
    match a.submit("/import foo.json".into()) {
        Some(Action::Slash { line }) => assert_eq!(line, "import foo.json"),
        other => panic!("an unknown verb must reach the daemon, got {other:?}"),
    }
    // No verdict from this head: the notice is not set, because the answer is the
    // daemon's to give and it arrives on the session log.
    assert!(
        a.input().is_empty(),
        "the head kept the line instead of sending it: {:?}",
        a.input()
    );

    // **A head verb is still the head's** and is not forwarded — the arms above
    // the fallthrough keep it, which is what makes the fallthrough safe.
    let mut b = app();
    b.session_id = "s1".into();
    assert_eq!(b.submit("/status".into()), None, "/status opens a pane");
    assert!(b.stats, "and it really opened it");
    assert_eq!(b.submit("/quit".into()), Some(Action::Quit));
    // …and `models` with no argument is the menu, not a daemon round trip.
    let mut c = app();
    c.session_id = "s1".into();
    assert!(matches!(c.submit("/models".into()), Some(Action::Settings)));
    assert!(c.pick == Some(Pick::Model), "the menu opened");
}

/// **Up recalls the session's prompts, not only this head's.** The operator: *"i worked -
/// sent 30 prompts. then restart, send 2. and arrow up sees only these two"*. A fresh
/// head attached to a session with earlier prompts recalls them, newest first, after
/// the ones it typed itself; another speaker's rows (a harness notice) are not prompts.
#[test]
fn up_recalls_the_sessions_earlier_prompts_after_a_restart() {
    let mut a = app();
    let user = |text: &str, speaker: letibot_transcript::Speaker| TranscriptItem::User {
        parts: vec![UserPart::Text { text: text.into() }],
        speaker,
    };
    let rows = [
        (
            "u.0",
            user(
                "first prompt of yesterday",
                letibot_transcript::Speaker::Operator,
            ),
        ),
        (
            "u.1",
            user("a harness notice", letibot_transcript::Speaker::Agent),
        ),
        (
            "u.2",
            user(
                "! ./build/stroppy help",
                letibot_transcript::Speaker::Operator,
            ),
        ),
        (
            "u.3",
            user(
                "last prompt before restart",
                letibot_transcript::Speaker::Operator,
            ),
        ),
    ];
    for (i, (id, item)) in rows.into_iter().enumerate() {
        let n = i as u64 * 2;
        a.apply(ServerFrame::Event(env(
            n + 1,
            testing::appended(id, "user"),
        )));
        a.apply(ServerFrame::Event(env(
            n + 2,
            SessionEvent::TranscriptContent {
                item_id: id.into(),
                item: Box::new(item),
            },
        )));
    }
    // Nothing typed in this head yet: Up walks the session's own prompts.
    a.key(Key::Up);
    assert_eq!(a.input(), "last prompt before restart");
    a.key(Key::Up);
    assert_eq!(a.input(), "! ./build/stroppy help");
    a.key(Key::Up);
    assert_eq!(
        a.input(),
        "first prompt of yesterday",
        "the notice is not a prompt"
    );
}

#[test]
fn the_body_a_frame_builds_does_not_grow_with_the_session() {
    // §13.3, at the renderer rather than at the lexer. The old shape cloned the
    // whole history into every frame; this asserts the frame is the window.
    let mut a = app();
    for i in 0..400u64 {
        a.apply(ServerFrame::Event(env(
            i * 2 + 1,
            testing::appended(&format!("s.{i}"), "user"),
        )));
        a.apply(ServerFrame::Event(env(
            i * 2 + 2,
            testing::content(&format!("s.{i}"), "a line of conversation"),
        )));
    }
    let screen = a.screen(80, 24);
    assert_eq!(screen.len(), 24);
    assert!(a.body_len > 400, "the history is there: {}", a.body_len);
}

/// **The live `!` row draws only the candidates that fit.**
///
/// The cut is made while collecting rather than by `trim_to` at the end, because the
/// collection is where the cost was: a session with two thousand matching commands
/// cloned and joined every one of them on every frame to produce a row that was then
/// thrown away down to the width. What a reader sees is what they always saw — the
/// candidates that fit, newest first, and no marker for the rest.
#[test]
fn the_bang_row_draws_only_the_candidates_that_fit() {
    let mut a = app();
    for i in 0..200u64 {
        shell_row(
            &mut a,
            i * 2 + 1,
            &format!("s.{i}"),
            "assistant",
            bash_row(&format!("cargo test {i}")),
        );
    }
    typed(&mut a, "! cargo");
    let row = a.shell_completions_line(40).expect("the row");
    assert!(visible_width(&row) <= 40, "the row fits: {row}");
    assert!(row.contains("! cargo test 199"), "the newest leads: {row}");
    assert!(
        !row.contains("! cargo test 0"),
        "and the far end is never collected: {row}"
    );
}

#[test]
fn rendering_the_history_does_not_grow_with_the_session_either() {
    let mut a = app();
    let n = 400u64;
    for i in 0..n {
        a.apply(ServerFrame::Event(env(
            i * 2 + 1,
            testing::appended(&format!("s.{i}"), "user"),
        )));
        a.apply(ServerFrame::Event(env(
            i * 2 + 2,
            testing::content(&format!("s.{i}"), "a line of conversation"),
        )));
        // A frame per event, which is what the driver does: the walk has to
        // have caught up before the next row lands or nothing is re-rendered.
        let _ = a.screen(80, 24);
    }
    assert!(
        a.hist_renders < 4 * n,
        "{n} rows cost {} row renders; a full rebuild per row would be about {}",
        a.hist_renders,
        n * n / 2
    );
}

/// **`!send LINE` is a verb, and it is checked before the bare `!`.**
///
/// `!send Y` is also a perfectly good `!` line — `operator_shell_command` reads it as the
/// command `send Y`, which is a program nobody has — so the verb has to be taken first,
/// exactly as `!term` is and through the same kind of shared parse
/// (`letibot_sessionlog::send_line`). And the line leaves with the verb stripped, so the
/// daemon is handed the text and not the spelling.
///
/// **A bare `!send` sends an empty line**, which is an Enter and a real answer; and
/// `!sender`, `!sends` and `!send-mail` are ordinary `!` lines and always were.
#[test]
fn the_send_verb_is_recognised_before_the_bare_bang() {
    let mut a = app();
    assert_eq!(
        a.submit("!send Y".into()),
        Some(Action::SendLine { line: "Y".into() })
    );
    assert_eq!(
        a.submit("!send".into()),
        Some(Action::SendLine {
            line: String::new()
        }),
        "a bare `!send` is an Enter, which is a real answer"
    );
    assert_eq!(
        a.submit("!send yes please".into()),
        Some(Action::SendLine {
            line: "yes please".into()
        }),
        "the text is one line and is not re-split"
    );
    // **No echo.** A `!` line leaves an echo because a `User` row is coming to retire it;
    // a `!send` line goes into a running program's stdin and leaves no row behind, so an
    // echo would be this head claiming a line the transcript will never carry.
    assert!(
        a.pending_prompts.is_empty(),
        "a `!send` must not join the pending prompts: {:?}",
        a.pending_prompts
    );
    // The near misses fall through to the operator's own shell line, which is what they
    // always were.
    for line in ["!sender x", "!sends x", "!send-mail"] {
        assert!(
            matches!(a.submit(line.into()), Some(Action::OperatorShell { .. })),
            "`{line}` is an ordinary `!` line"
        );
    }
}

/// **§1.7: a question can be answered.**
///
/// The requirement's own words for this head are *"cannot answer one at all"*, and
/// the measurement behind that is worth restating because it is not obvious from
/// either the type or the screen: a question carries **`options: []` with the model's
/// offered choices in `choices`** (`Vec<String>`), and every row of this head's
/// ladder machinery asked `options.len()` — so the bound was zero, the arrows had
/// nothing to wrap on, `answer_marked` returned `None`, and a typed line was held
/// with *"this ask offers no options"* while the card drew an empty ladder under a
/// question the model had offered three answers to.
///
/// D10's three fields, one at a time: **index**, **the choice's own text**, and
/// **free text** — the last being *"a typed answer, and a first-class one"*, named
/// against Claude Code's *"chat later"*.
#[test]
fn a_question_is_drawn_with_its_choices_and_can_be_answered_three_ways() {
    use letibot_sessionlog::event::OnTimeout;
    let question = |choices: &[&str]| {
        let mut d = decision_with(&[]);
        d.kind = "question".into();
        d.summary = "which database should the migration target?".into();
        d.choices = choices.iter().map(|c| (*c).to_string()).collect();
        d.on_timeout = OnTimeout::Deny;
        d
    };
    let one = question(&["postgres", "sqlite", "a new one"]);

    // **The card draws the model's choices**, and it no longer claims the ask
    // offered none — which is what it said while drawing an empty ladder.
    let a = app();
    let card = a.decision_lines(&one, 200).join("\n");
    assert!(
        card.contains("which database should the migration target?"),
        "{card}"
    );
    for c in ["postgres", "sqlite", "a new one"] {
        assert!(
            card.contains(c),
            "the choice {c:?} is not on the card: {card}"
        );
    }
    // A question's rows are prose, so there is no `(option_id)` beside them — and
    // the hint offers the free answer rather than naming an id the question has not
    // got.
    assert!(card.contains("or type your own answer"), "{card}");
    assert!(!card.contains("type the id"), "{card}");

    // **1. By index, with the arrows and Enter.** The marker is the answer, the same
    // contract every other ladder in this file keeps.
    let mut a = app();
    a.open.push(one.clone());
    assert_eq!(a.sel, 0);
    assert_eq!(a.key(Key::Down), None, "moving is not answering");
    assert_eq!(a.sel, 1);
    match a.key(Key::Enter) {
        Some(Action::AnswerQuestion { req_id, answer }) => {
            assert_eq!(req_id, "d1");
            assert_eq!(answer.option, Some(1));
            assert_eq!(answer.free, None, "an index answer says no words");
        }
        other => panic!("the marked row is the answer, got {other:?}"),
    }

    // **2. By the choice's own text**, which is how a person answers a menu they
    // were shown. Answered as an INDEX rather than as free prose, because the model
    // should get back which of the three it offered.
    let mut a = app();
    a.open.push(one.clone());
    match a.submit("sqlite".into()) {
        Some(Action::AnswerQuestion { answer, .. }) => {
            assert_eq!(answer.option, Some(1), "{answer:?}");
            assert_eq!(answer.free, None, "{answer:?}");
        }
        other => panic!("a choice's own text is that choice, got {other:?}"),
    }
    // Case and surrounding whitespace are a person's, not a spelling test — but
    // the *words* still have to be the choice's. `PostgreSQL` is not `postgres`
    // (ten characters against eight), so it is free text; `  SQLITE  ` is the
    // second choice wearing a person's casing and spacing.
    let mut a = app();
    a.open.push(one.clone());
    match a.submit("  SQLITE  ".into()) {
        Some(Action::AnswerQuestion { answer, .. }) => {
            assert_eq!(answer.option, Some(1), "{answer:?}")
        }
        other => panic!("got {other:?}"),
    }
    let mut a = app();
    a.open.push(one.clone());
    match a.submit("PostgreSQL".into()) {
        Some(Action::AnswerQuestion { answer, .. }) => {
            assert_eq!(
                answer.option, None,
                "not a choice, so it is words: {answer:?}"
            );
            assert_eq!(answer.free.as_deref(), Some("PostgreSQL"));
        }
        other => panic!("got {other:?}"),
    }

    // **3. Free text** — a sentence the model did not offer. This is the field the
    // requirement names as first-class, and the one the old code path could not
    // reach at all: it held the words and answered the marked row instead.
    let mut a = app();
    a.open.push(one.clone());
    match a.submit("neither — split it into two migrations".into()) {
        Some(Action::AnswerQuestion { answer, .. }) => {
            assert_eq!(answer.option, None);
            assert_eq!(
                answer.free.as_deref(),
                Some("neither — split it into two migrations")
            );
        }
        other => panic!("a typed sentence is an answer, got {other:?}"),
    }
    assert!(
        a.open.len() == 1,
        "the head holds the words rather than answering: {:?}",
        a.input()
    );

    // **And a question whose model offered nothing is still answerable in words.**
    // This is the boundary the four call sites have to agree about: no rows, so no
    // marked answer and no digits — but the composer still answers.
    let bare = question(&[]);
    let mut a = app();
    a.open.push(bare.clone());
    assert_eq!(a.key(Key::Enter), None, "there is no marked row to take");
    assert_eq!(a.key(Key::Char('2')), None, "and no row 2");
    match a.submit("do whatever you think is best".into()) {
        Some(Action::AnswerQuestion { answer, .. }) => {
            assert_eq!(answer.option, None, "{answer:?}");
            assert_eq!(
                answer.free.as_deref(),
                Some("do whatever you think is best")
            );
        }
        other => panic!("got {other:?}"),
    }
    // An empty line on a question with no choices answers nothing rather than
    // sending an empty answer the daemon would refuse — a refusal the head can
    // predict is a keystroke thrown away.
    let mut a = app();
    a.open.push(bare);
    assert_eq!(a.submit(String::new()), None);
}

/// **A child whose completion arrived does not count.**
///
/// The other end of the same rule: the daemon's `done` is the completion and it is the only
/// thing that may end a child's life in this pane. `answer` is `Some` on that event and on no
/// other, so the row and the count agree about what has ended without either of them reading
/// a clock, an instant, or the absence of one.
///
/// **The list is the one frame the completion outlives**, and it is the frame this asserts in:
/// a brief saying *a turn is generating in that session now* still overrides a settled row —
/// *"a child asked for more work has started a second turn"*, which
/// `the_session_list_owns_the_word_running_…` holds and this change does not disturb. The list
/// is a positive measurement of life, and nothing here may retire a live child; what changed is
/// that a **negative** one may no longer end a child that has not completed.
#[test]
fn a_child_whose_completion_arrived_does_not_count() {
    let mut a = app();
    // The child has ended, and the daemon's list agrees: no turn is generating in it.
    let mut list = a_family();
    list[1].status.running = false;
    a.apply(hello("s", list, Hub::new("s").snapshot()));
    a.apply(ServerFrame::Event(env(
        1,
        child_event("s-sub-1", "running", None),
    )));
    assert!(
        count_row(&mut a).contains("1 subagent running"),
        "the premise: the child is up and counted"
    );
    // The completion, carrying the child's answer's first line.
    a.apply(ServerFrame::Event(env(
        2,
        child_event("s-sub-1", "done", Some("3529 files")),
    )));
    assert_eq!(a.subagents[0].state, "done");
    assert_eq!(a.subagents[0].answer.as_deref(), Some("3529 files"));
    assert!(a.subagents[0].is_finished());
    let screen = a.screen(100, 24).join("\n");
    assert!(
        !screen.contains("subagent running"),
        "a finished child is still counted:\n{screen}"
    );
    // **And it is drawn where finished children go**, under the fold — with `s-sub-2`, the
    // child this head never watched, which the list rebuilt and which is finished too.
    a.key(Key::CtrlG);
    let pane = a.screen(100, 24).join("\n");
    assert!(pane.contains("finished (2)"), "{pane}");
    assert!(
        !pane.contains("[~]"),
        "a finished child wears a live mark:\n{pane}"
    );
}

/// **A settlement line is one line, however long the fact inside it is.**
///
/// MEASURED on this head, 2026-10-05, by the operator looking at it: *"a giant prompt"*.
/// The `task` this head gives a subagent is a brief of thousands of characters, and
/// [`folded_notice`] folds it into the settlement line because what a child was *asked* is the
/// one fact the notice cannot say for itself. Pasted in whole it drew five wrapped rows of
/// somebody's instructions where a settlement should be one line.
///
/// The trim is to the width the block will draw it in — [`session_text_cols`], the same number
/// `session_block` wraps to — so what the reader gets is one row ending in the head's own `…`
/// rather than a paragraph. The pane keeps the same rule at its own width (its rows end in
/// `trim_to`), and the whole of the task is in the child's own session, which is where a reader
/// goes for the whole of it.
#[test]
fn a_completion_row_holds_a_whole_brief_on_one_line() {
    const BRIEF: &str = "You are implementing one feature in the letibot repository, in your own git worktree. Read this whole brief before touching anything. The repo is at ~/Projects/letibot, the workspace root you are in, and main must not be touched.";
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    // **The row after the `Hello`**, because attaching rewrites the pane: a child this head
    // watched spawn is what `SubagentState` is for, and the fixture has to be in the state the
    // live head is in when the notice arrives.
    a.subagents = vec![SubagentState {
        session_id: "s-sub-1".into(),
        state: "failed".into(),
        generating: false,
        prompt: String::new(),
        role: "coder".into(),
        task: BRIEF.into(),
        model: String::new(),
        answer: None,
        spawned_ms: 0,
    }];
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::TranscriptAppended {
            item_id: "s.0".into(),
            kind: "user".into(),
            ledger_head: String::new(),
        },
    )));
    a.apply(ServerFrame::Event(env(
            2,
            SessionEvent::TranscriptContent {
                item_id: "s.0".into(),
                item: Box::new(TranscriptItem::User {
                    speaker: letibot_transcript::Speaker::Agent,
                    parts: vec![letibot_transcript::UserPart::Text {
                        text: "[task] a subagent you started has finished:\n  - `s-sub-1` failed: the cap is spent\nThis is the completion arriving on its own — you do not need to wait for it.".into(),
                    }],
                }),
            },
        )));
    let screen = a.screen(100, 30);
    let rows: Vec<&String> = screen.iter().filter(|l| l.contains("s-sub-1")).collect();
    assert_eq!(
        rows.len(),
        1,
        "the settlement drew {} rows instead of one:\n{:#?}",
        rows.len(),
        rows
    );
    let row = rows[0];
    assert!(row.contains("You are implementing one feature"), "{row:?}");
    assert!(row.contains("session · "), "{row:?}");
    assert!(row.contains('…'), "the line was not elided: {row:?}");
    assert!(
        !screen
            .iter()
            .any(|l| l.contains("main must not be touched")),
        "the tail of the brief reached the glass:\n{}",
        screen.join("\n")
    );
}

/// **A draft in the composer survives an ask on BOTH keys** — R51 items 13/14's neighbour, and
/// leticl's `1f48056`.
///
/// The brief names `↑`; the same failure lives on Enter, and this is the pair of them measured
/// rather than reasoned about. Enter on a permission whose row the line does not name HOLDS the
/// words (`a_line_that_names_nothing_holds_the_words_and_answers_the_marked_row` is the other
/// half); `↑` must not recall a queued line over a draft either — readline's own history is
/// what a half-typed line gets.
#[test]
fn an_open_ask_does_not_take_the_draft_on_enter_or_on_up() {
    use letibot_sessionlog::event::OptionKind;
    // Up, with a draft: the composer's own history walk, and the draft stays.
    let mut a = app();
    a.open.push(decision_with(&[
        OptionKind::AllowOnce,
        OptionKind::RejectOnce,
    ]));
    typed(&mut a, "half a thought");
    a.key(Key::Up);
    assert_eq!(
        a.input(),
        "half a thought",
        "the card's ladder moved and took the draft with it"
    );
    // The queued line is recalled only onto an EMPTY composer, so it cannot eat a draft.
    let mut b = app();
    b.open.push(decision_with(&[
        OptionKind::AllowOnce,
        OptionKind::RejectOnce,
    ]));
    b.pending_prompts.push("a queued line".into());
    typed(&mut b, "half a thought");
    assert!(
        matches!(b.key(Key::Up), None),
        "a draft's Up is not the queue's recall"
    );
    assert_eq!(b.input(), "half a thought", "and the draft is untouched");
    assert_eq!(
        b.pending_prompts,
        vec!["a queued line".to_string()],
        "the queue was not taken back by a key that did not recall it"
    );

    // **And with NO card up, which is where the recall is actually reachable** — and this is
    // the assertion the two above cannot make: a card with options owns `↑` for its ladder
    // (proved by removing the empty-composer guard and watching the two above still pass), so
    // the guard is only ever exercised on the bare composer.
    let mut c = app();
    c.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    c.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    typed(&mut c, "a sent line");
    c.key(Key::Enter);
    c.apply(ServerFrame::Event(env(
        2,
        SessionEvent::TranscriptAppended {
            item_id: "s.0".into(),
            kind: "user".into(),
            ledger_head: String::new(),
        },
    )));
    // The entry the send left behind is stood down by hand, so the premise below is about ONE
    // queued line rather than about the sent one the coalescing already folded it into.
    c.pending_prompts.clear();
    c.pending_prompts.push("a queued line".into());
    // With nothing typed, `↑` IS the recall — the premise, so the refusal below is about the
    // draft and not about the key being dead.
    c.key(Key::Up);
    assert_eq!(c.input(), "a queued line", "the premise: the recall works");
    assert!(
        c.pending_prompts.is_empty(),
        "the premise: and it takes it back"
    );
    // Now with a draft: readline's own history, and the queue is not touched.
    //
    // **The draft is not lost, and that is the editor's contract rather than this head's** —
    // §6: a history walk keeps the draft it interrupted, and `↓` brings it back. What this
    // asserts is the half that IS this head's, and the half the brief is about: `↑` over a
    // draft is NOT the queue's recall, so it can neither eat the draft nor take back a prompt
    // the operator did not ask for.
    c.set_composer("");
    c.pending_prompts.push("a queued line".into());
    typed(&mut c, "half a thought");
    assert!(
        matches!(c.key(Key::Up), None),
        "a draft's Up is not the queue's recall"
    );
    assert_eq!(
        c.input(),
        "a sent line",
        "the draft's Up is readline's: the previous entry"
    );
    assert_eq!(
        c.pending_prompts,
        vec!["a queued line".to_string()],
        "and the queue is not taken back on the way"
    );
    assert!(
        matches!(c.key(Key::Down), None),
        "Down walks back out of the history"
    );
    assert_eq!(c.input(), "half a thought", "the draft survived the walk");
}

#[test]
fn tab_completes_a_slash_command_and_more_tabs_cycle_the_matches() {
    let mut a = app();
    a.editor.insert("/se");
    assert_eq!(a.key(Key::Tab), None);
    assert_eq!(a.input(), "/sessions");
    // "/s" matches three commands; the second Tab walks the cycle in table
    // order, and the cycle wraps.
    a.set_composer("/s");
    a.completion = None;
    a.key(Key::Tab);
    assert_eq!(a.input(), "/sessions");
    a.key(Key::Tab);
    assert_eq!(a.input(), "/switch");
    a.key(Key::Tab);
    assert_eq!(a.input(), "/status");
    // **A §6 verb joined the cycle**, which is the point of listing it: `/s` reaches
    // `subagents` by Tab now, and the expectation has to name it or the test is
    // pinning a list that no longer exists.
    a.key(Key::Tab);
    assert_eq!(a.input(), "/subagents");
    // **And R32's two**, which this test caught the moment they were listed — the
    // mechanism working rather than a nuisance. `/settings` and `/stats` are the
    // dispatcher's own aliases for `/config` and `/status`; both worked and neither was
    // offered until the table gained them, which is the measured finding
    // (`docs/evidence/slash-completion-2026-09-23.py`).
    a.key(Key::Tab);
    assert_eq!(a.input(), "/settings");
    a.key(Key::Tab);
    assert_eq!(a.input(), "/stats");
    a.key(Key::Tab);
    assert_eq!(a.input(), "/sessions", "the cycle wraps");
    // A character typed on after a completion kills the cycle: the next
    // Tab matches fresh, and must not clobber what was typed.
    a.set_composer("/switch");
    a.completion = Some(("/sw".into(), vec!["switch".to_string()], 0));
    a.editor.insert("i");
    assert_eq!(a.input(), "/switchi");
    a.key(Key::Tab);
    assert_eq!(a.input(), "/switchi", "no match, so nothing changed");
    // A prefix nothing matches is refused where it stands.
    a.set_composer("/zz");
    a.completion = None;
    a.key(Key::Tab);
    assert_eq!(a.input(), "/zz");
    assert!(a.notice.is_some(), "the refusal is said, not silent");
    // The live row above the composer lists the matches while typing.
    a.set_composer("/s");
    a.completion = None;
    let screen = a.screen(110, 24);
    assert!(
        screen
            .iter()
            .any(|l| l.contains("/sessions") && l.contains("/switch")),
        "the live completions row shows the matches:\n{}",
        screen.join("\n")
    );
}

/// **A `!` line completes from what this session has actually run** — the
/// operator's own `!` rows and the model's `bash` calls both become
/// candidates, which is the feature in one sentence: *"smart autocomplete
/// here for ! - you trying to suggest me commands based on conversation
/// context"*.
#[test]
fn a_bang_line_completes_from_the_sessions_own_rows() {
    let mut a = app();
    shell_row(
        &mut a,
        1,
        "u1",
        "user",
        TranscriptItem::User {
            speaker: letibot_transcript::Speaker::Operator,
            parts: vec![UserPart::Text {
                text: "! ls -la".into(),
            }],
        },
    );
    shell_row(
        &mut a,
        3,
        "a1",
        "assistant",
        TranscriptItem::Assistant {
            text: String::new(),
            tool_calls: vec![letibot_transcript::ToolCall {
                id: "c1".into(),
                name: "bash".into(),
                arguments: r#"{"command": "git status"}"#.into(),
            }],
            truncated: false,
        },
    );
    // The operator's own row is a candidate.
    typed(&mut a, "! ls");
    a.key(Key::Tab);
    assert_eq!(
        a.input(),
        "! ls -la",
        "the operator's own row is a candidate"
    );
    // The model's bash call is a candidate, as `! ` plus the command.
    a.set_composer("! git");
    a.completion = None;
    a.key(Key::Tab);
    assert_eq!(
        a.input(),
        "! git status",
        "the model's bash call is a candidate"
    );
}

/// **The newest candidate wins the first Tab, and a second Tab cycles.**
///
/// The last thing the session ran is the most likely thing the operator is
/// about to run again, so it is first in the cycle; Tab again walks the rest.
/// **The cycle does not wrap any more**: when it is exhausted the model is asked,
/// about the line in the composer. See
/// [`App::shell_model_fallback`] and the tests below it.
#[test]
fn the_newest_bang_candidate_wins_and_tab_cycles() {
    let mut a = app();
    shell_row(&mut a, 1, "u1", "user", operator_row("! ls -la"));
    shell_row(&mut a, 3, "u2", "user", operator_row("! ls ."));
    // `! ls` matches both; the newest (`! ls .`) wins the first Tab.
    typed(&mut a, "! ls");
    a.key(Key::Tab);
    assert_eq!(a.input(), "! ls .", "the newest candidate wins");
    // A second Tab cycles to the older one.
    a.key(Key::Tab);
    assert_eq!(a.input(), "! ls -la", "the second Tab cycles");
    // The third Tab exhausts the history, and the hand-off is the model's: the
    // composer stays on the last candidate the history offered — nothing the
    // operator has just cycled past is shown again — and the ask is for THAT line,
    // because that is the line being completed.
    a.key(Key::Tab);
    assert_eq!(
        a.input(),
        "! ls -la",
        "the composer stays on the last candidate"
    );
    let (prefix, id) = the_one_ask(&mut a);
    assert_eq!(prefix, "! ls -la", "asked about the line in the composer");
    // And the answer is cycled rather than the history's wrapping back.
    model_answers(&mut a, &id, "! ls -la", &["! ls -la --color"]);
    a.key(Key::Tab);
    assert_eq!(
        a.input(),
        "! ls -la --color",
        "the model's line is the cycle"
    );
}

/// **The history is the first answer, and the model is not asked when it matches.**
///
/// The whole shape of the feature: a command this session actually ran is a fact
/// and a model's proposal is a guess, so the fact answers first and the guess is
/// never paid for. A head that asked anyway would spend a local model call on every
/// Tab, which is the cost the history-first rule exists to avoid.
#[test]
fn the_history_is_the_first_answer_and_the_model_is_not_asked_when_it_matches() {
    let mut a = app();
    shell_row(&mut a, 1, "u1", "user", operator_row("! ls -la"));
    typed(&mut a, "! ls");
    a.key(Key::Tab);
    assert_eq!(a.input(), "! ls -la", "the session's own row answers");
    assert!(a.take_actions().is_empty(), "and no model was asked");
}

/// **A prefix the history does not have asks the model once, and only once.**
///
/// The operator's ask, in their words: *"i want smart ! when a model suggest
/// completions."* The composer is untouched while the ask is out — nothing is
/// filled from a guess that has not arrived — and a second Tab in that state is not
/// a second call: the ask is filed under the id the head minted, and the head is
/// what recognises the answer.
#[test]
fn the_model_is_asked_once_for_a_prefix_the_history_does_not_have() {
    let mut a = app();
    shell_row(&mut a, 1, "u1", "user", operator_row("! ls -la"));
    typed(&mut a, "! git");
    a.key(Key::Tab);
    assert_eq!(a.input(), "! git", "the composer is untouched while asking");
    let (prefix, id) = the_one_ask(&mut a);
    assert_eq!(prefix, "! git");
    assert!(a.notice.is_some(), "the wait is said, not silent");
    assert!(
        a.shell_ask.contains_key(&id),
        "filed under the id the head minted, which is the one it recognises"
    );
    a.key(Key::Tab);
    assert!(a.take_actions().is_empty(), "the same prefix is one call");
    assert_eq!(a.input(), "! git");
}

/// **The answer is cached by (prefix, position), cycled, and never asked twice.**
///
/// `shell_ask` is the asks in flight and `shell_suggestions` the answers, and the
/// rule both exist for is in one sentence: the same prefix at the same transcript
/// position is one model call. The operator may press Tab as often as they like.
#[test]
fn the_models_answer_is_cached_and_cycled_and_the_same_prefix_is_one_call() {
    let mut a = app();
    shell_row(&mut a, 1, "u1", "user", operator_row("! ls -la"));
    typed(&mut a, "! git");
    a.key(Key::Tab);
    let (_, id) = the_one_ask(&mut a);
    model_answers(&mut a, &id, "! git", &["! git status", "! git log"]);
    // The ask is retired and the answer is cached under the prefix it was for.
    assert!(a.shell_ask.is_empty(), "the ask was answered");
    assert_eq!(a.shell_suggestions.len(), 1, "and the answer is held");
    // The first Tab after the answer fills the composer with the model's first
    // line, and asks nothing.
    a.key(Key::Tab);
    assert_eq!(a.input(), "! git status", "the model's first line");
    assert!(a.take_actions().is_empty(), "answered from the cache");
    a.key(Key::Tab);
    assert_eq!(a.input(), "! git log", "the second Tab cycles");
    a.key(Key::Tab);
    assert_eq!(a.input(), "! git status", "and the model's own cycle wraps");
    assert!(a.take_actions().is_empty(), "still no call");
    // Back at the typed prefix: the cache is keyed by it, so this is the same ask
    // and not a third call.
    a.set_composer("! git");
    a.shell_model = None;
    a.key(Key::Tab);
    assert_eq!(
        a.input(),
        "! git status",
        "the same prefix, the same answer"
    );
    assert!(
        a.take_actions().is_empty(),
        "the same prefix at the same position is one call"
    );
}

/// **A row landing makes the model's suggestion stale.**
///
/// A suggestion is built on the conversation as it was when it was asked, so a
/// conversation that moved is a different question — and the answer to the old one
/// is not an answer to the new one. The cache and the cycle both go with the rows,
/// and the same prefix is a fresh ask rather than a stale answer.
#[test]
fn a_row_landing_makes_the_models_suggestion_stale() {
    let mut a = app();
    shell_row(&mut a, 1, "u1", "user", operator_row("! ls -la"));
    typed(&mut a, "! git");
    a.key(Key::Tab);
    let (_, id) = the_one_ask(&mut a);
    model_answers(&mut a, &id, "! git", &["! git status"]);
    assert_eq!(a.shell_suggestions.len(), 1, "the answer is cached");
    // A row lands.
    shell_row(
        &mut a,
        3,
        "a1",
        "assistant",
        TranscriptItem::Assistant {
            text: "done".into(),
            tool_calls: Vec::new(),
            truncated: false,
        },
    );
    assert!(
        a.shell_suggestions.is_empty(),
        "the cache went with the conversation it was built on"
    );
    assert!(a.shell_model.is_none(), "and so did the cycle");
    // The same prefix is a fresh ask at the new position, and the stale line never
    // reaches the composer.
    a.set_composer("! git");
    a.key(Key::Tab);
    assert_eq!(
        a.input(),
        "! git",
        "the stale suggestion does not fill the composer"
    );
    let (prefix, _) = the_one_ask(&mut a);
    assert_eq!(prefix, "! git", "asked again, at the new position");
}

/// **An empty answer is an answer, and it is said once.**
///
/// The model was told that a wrong suggestion is worse than none, so *nothing* is
/// the answer it is supposed to be able to give — and a cache that read an empty
/// list as *not asked* would send a fresh call on every Tab for the same prefix,
/// which is exactly what the (prefix, position) key exists to prevent.
#[test]
fn an_empty_answer_is_said_once_and_is_not_asked_for_again() {
    let mut a = app();
    shell_row(&mut a, 1, "u1", "user", operator_row("! ls -la"));
    typed(&mut a, "! git");
    a.key(Key::Tab);
    let (_, id) = the_one_ask(&mut a);
    model_answers(&mut a, &id, "! git", &[]);
    for _ in 0..3 {
        a.key(Key::Tab);
        assert_eq!(a.input(), "! git", "nothing is filled from nothing");
        assert!(a.take_actions().is_empty(), "an empty answer is an answer");
    }
    assert!(
        a.notice.as_deref().is_some_and(|n| n.contains("no ! line")),
        "and it is said plainly: {:?}",
        a.notice
    );
}

/// **The model's lines are drawn as proposals and the history's as facts.**
///
/// A line that looks like the operator typed it and did not is the same class of lie
/// as an unattributed quote, so the live row marks the model's candidates and leaves
/// the session's own plain. The mark is display-only: a Tab fills the composer with
/// the line itself.
///
/// The state both kinds are drawn in is the hand-off: the history answered with its
/// own line, the operator cycled past it, and the model was asked about the line
/// they are now looking at.
#[test]
fn the_model_lines_are_drawn_as_proposals_and_the_historys_as_facts() {
    let mut a = app();
    shell_row(&mut a, 1, "u1", "user", operator_row("! ls -la"));
    typed(&mut a, "! ls");
    // The history answers first, with the line it actually ran.
    a.key(Key::Tab);
    assert_eq!(a.input(), "! ls -la", "the session's own line");
    // The next Tab exhausts the history and asks the model about that line.
    a.key(Key::Tab);
    let (prefix, id) = the_one_ask(&mut a);
    assert_eq!(prefix, "! ls -la");
    model_answers(
        &mut a,
        &id,
        "! ls -la",
        &["! ls -la --color", "! ls -la -R"],
    );
    // The live row carries both kinds, told apart: the session's own plain, the
    // model's marked. This is the requirement in one string.
    let row = a
        .shell_completions_line(110)
        .expect("the live row above the composer");
    assert_eq!(
        row, "  ! ls -la  ·  ~! ls -la --color  ·  ~! ls -la -R",
        "the history's plain and the model's marked"
    );
    // And it is on the screen, not merely computed.
    let screen = a.screen(110, 24).join("\n");
    assert!(screen.contains("~! ls -la --color"), "drawn: {screen}");
    // The mark never reaches the composer: a Tab fills the line itself.
    a.key(Key::Tab);
    assert_eq!(a.input(), "! ls -la --color", "the line itself, unmarked");
}

/// **A suggestion only fills the composer.** Nothing in the path submits: a Tab
/// replaces the line, and Enter — the operator's own key — is what sends it.
#[test]
fn a_suggestion_only_fills_the_composer_and_never_submits() {
    let mut a = app();
    shell_row(&mut a, 1, "u1", "user", operator_row("! ls -la"));
    typed(&mut a, "! git");
    a.key(Key::Tab);
    let (_, id) = the_one_ask(&mut a);
    model_answers(&mut a, &id, "! git", &["! git status"]);
    a.key(Key::Tab);
    assert_eq!(a.input(), "! git status");
    assert!(a.take_actions().is_empty(), "the Tab sent nothing");
    assert!(a.pending_prompts.is_empty(), "and nothing is held as sent");
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::OperatorShell {
            line: "! git status".into()
        }),
        "Enter is still the operator's"
    );
}

/// **The suggestion row is a SLOT, so the transcript does not move under the reader.**
///
/// The operator, watching the pane while they typed a `!` line: *"the conversation jumps
/// one line up and then down"*. The frame's arithmetic is why — the conversation is given
/// `h` minus the chrome, so a completion row that came and went took its line from the
/// transcript and handed it back, once per appearance and dismissal of the candidate list,
/// while the reader was typing.
///
/// **What is pinned is the MOVE, not the row.** Asserting *the row is always there* would
/// pass on a frame that reserved it in the wrong place; what a reader feels is the shove,
/// so this compares the transcript's own last line, the conversation's rows and the
/// composer's top edge across three frames — candidates, none, candidates again. The
/// fixture FILLS the screen on purpose: a short transcript sits at the top, the row it
/// loses comes out of the blank space under it, and the shove cannot be seen at all (which
/// is how the first version of `a_turn_starting_does_not_shove_the_transcript_up_a_row`
/// passed against a reverted fix).
///
/// **And the slot is still the typing aid it was**: the layout change may not cost the
/// completion, so the last act here is a Tab.
#[test]
fn the_suggestions_are_in_the_bottom_edge_and_nothing_moves() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    // A command this session actually ran, so `! cargo` has a candidate and one character
    // further on — `! cargo x` — has none.
    shell_row(&mut a, 1, "u1", "user", operator_row("! cargo test 199"));
    for i in 0..30u64 {
        // Contiguous sequence numbers: a gap here is a resync notice in the middle of
        // the transcript, and this fixture wants the transcript and nothing else.
        let (seq, id, text) = (
            i + 2,
            format!("s.{i}"),
            format!("row {i} of the transcript"),
        );
        a.apply(ServerFrame::Event(env(
            seq,
            testing::appended(&id, "assistant"),
        )));
        a.record_item(
            &id,
            TranscriptItem::Assistant {
                text,
                tool_calls: Vec::new(),
                truncated: false,
            },
        );
    }
    let composer_at = |f: &[String]| {
        f.iter()
            .position(|l| l.contains('╭'))
            .expect("the composer's top edge")
    };
    let last_row_at = |f: &[String]| {
        f.iter()
            .position(|l| l.contains("row 29 of the transcript"))
            .expect("the newest line of the transcript")
    };
    let conversation = |f: &[String]| {
        f.iter()
            .filter(|l| l.contains(" of the transcript"))
            .count()
    };
    let near_the_composer = |f: &[String]| {
        let top = composer_at(f);
        f[top.saturating_sub(3)..].join("\n")
    };

    let bottom_edge = |f: &[String]| {
        f.iter()
            .find(|l| l.contains('╰'))
            .cloned()
            .expect("the composer's bottom edge")
    };
    let empty = a.screen(100, 24);
    let top = composer_at(&empty);

    // 1. Candidates — **in the composer's bottom edge**, and nothing above it moved.
    typed(&mut a, "! cargo");
    let candidates = a.screen(100, 24);
    assert!(
        bottom_edge(&candidates).contains("! cargo test 199"),
        "the suggestion is drawn in the bottom edge:\n{}",
        near_the_composer(&candidates)
    );
    assert_eq!(
        composer_at(&candidates),
        top,
        "typing a completable line moved the composer:\n{}",
        near_the_composer(&candidates)
    );
    assert_eq!(
        last_row_at(&candidates),
        last_row_at(&empty),
        "**typing a completable line moved the conversation** — the operator, 2026-10-08: \
             *\"when i type / this gray hint line appears and conversation jumps one line\"*:\n{}",
        near_the_composer(&candidates)
    );
    assert_eq!(conversation(&candidates), conversation(&empty));

    // 2. **Dismissed** — one character on, where nothing matches: the edge goes plain, and
    //    still nothing moves.
    typed(&mut a, " x");
    let dismissed = a.screen(100, 24);
    assert_eq!(composer_at(&dismissed), top);
    assert_eq!(last_row_at(&dismissed), last_row_at(&empty));
    assert!(
        !bottom_edge(&dismissed).contains("cargo"),
        "nothing to suggest, and the edge says nothing:\n{}",
        near_the_composer(&dismissed)
    );

    // 3. And back — the reader who goes on typing and then backspaces must not watch the
    //    page move either way.
    a.key(Key::Backspace);
    a.key(Key::Backspace);
    let again = a.screen(100, 24);
    assert_eq!(composer_at(&again), top, "{:#?}", near_the_composer(&again));
    assert_eq!(last_row_at(&again), last_row_at(&empty));
    assert!(
        bottom_edge(&again).contains("! cargo test 199"),
        "and the candidates are back in the edge:\n{}",
        near_the_composer(&again)
    );

    // **The `/command` half is the same edge** — the door the operator reported.
    a.set_composer("/se");
    let slash = a.screen(100, 24);
    assert_eq!(composer_at(&slash), top, "{:#?}", near_the_composer(&slash));
    assert_eq!(last_row_at(&slash), last_row_at(&empty));
    assert!(
        bottom_edge(&slash).contains("/sessions"),
        "the matches are in the edge:\n{}",
        near_the_composer(&slash)
    );

    // **And the slot is still a completion.** Tab fills the line from it and sends
    // nothing, which is the behaviour the layout change was not allowed to cost.
    // The attach-time requests are drained first: what is asserted is that the Tab itself
    // adds nothing to them.
    let _ = a.take_actions();
    a.set_composer("! cargo");
    a.key(Key::Tab);
    assert_eq!(a.input(), "! cargo test 199", "Tab still completes");
    let actions = a.take_actions();
    assert!(actions.is_empty(), "and nothing was submitted: {actions:?}");
}

/// **A bare `!` is an empty command and does nothing** — the recogniser
/// (`operator_shell_command`) says so, the same rule the send refuses on.
/// A `!` prefix, by contrast, matches the candidates that start with it,
/// newest first.
#[test]
fn a_bare_bang_is_an_empty_command_and_does_nothing() {
    let mut a = app();
    shell_row(
        &mut a,
        1,
        "u1",
        "user",
        TranscriptItem::User {
            speaker: letibot_transcript::Speaker::Operator,
            parts: vec![UserPart::Text {
                text: "! ls -la".into(),
            }],
        },
    );
    // `!` alone is an empty command: the recogniser says so, and the
    // completion does nothing, the way the send refuses it.
    typed(&mut a, "!");
    a.key(Key::Tab);
    assert_eq!(
        a.input(),
        "!",
        "a bare bang is an empty command and does nothing"
    );
    // A `!` prefix matches the candidates that start with it, newest first.
    a.set_composer("! ls");
    a.completion = None;
    a.key(Key::Tab);
    assert_eq!(a.input(), "! ls -la", "a ! prefix matches, newest first");
}

/// **A `!` line with no history match leaves the text untouched** and says
/// so in the notice, the way `complete_slash` does.
#[test]
fn a_bang_line_with_no_history_match_leaves_the_text_untouched() {
    let mut a = app();
    shell_row(
        &mut a,
        1,
        "u1",
        "user",
        TranscriptItem::User {
            speaker: letibot_transcript::Speaker::Operator,
            parts: vec![UserPart::Text {
                text: "! ls -la".into(),
            }],
        },
    );
    // `! git` matches nothing this session ran.
    typed(&mut a, "! git");
    a.key(Key::Tab);
    assert_eq!(a.input(), "! git", "no match, so nothing changed");
    assert!(a.notice.is_some(), "the refusal is said, not silent");
}
