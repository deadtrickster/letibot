//! Background jobs: the pane, a job's output, settlements.

use super::*;

/// **The daemon's verbs are not this head's to enumerate** (R32), and the list it draws
/// comes from the row.
///
/// The measured defect, in one assertion: `/gate`, `/flowy`, `/job`, `/login` and
/// `/supervise` all work — they are forwarded — and none was offered, because the head
/// completed from a table that had never heard of them. With the row they are offered;
/// without it the head offers its own verbs and **says nothing about the rest**, which
/// is a daemon older than this one and not a guess.
#[test]
fn the_daemons_verbs_come_from_its_row_and_are_not_guessed_at() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    // No row yet: only this head's own verbs, and every one of them is dispatched.
    assert!(a.daemon_verbs().is_empty());
    let own = a.command_names();
    assert!(own.iter().any(|(n, _)| n == "compact"), "{own:?}");
    assert!(
        !own.iter().any(|(n, _)| n == "gate"),
        "a verb the daemon has not published must not be invented: {own:?}"
    );

    // Now the daemon answers, and its verbs join the list.
    a.apply(ServerFrame::Settings {
        rows: vec![letibot_sessionlog::protocol::SettingRow {
            key: letibot_sessionlog::protocol::DAEMON_VERBS_KEY.into(),
            value: "flowy,gate,job,login,supervise,tools".into(),
            source: "default".into(),
            editable: String::new(),
            choices: Vec::new(),
            tools: Vec::new(),
        }],
    });
    let all = a.command_names();
    for v in ["flowy", "gate", "job", "login", "supervise"] {
        assert!(
            all.iter().any(|(n, _)| n == v),
            "`/{v}` works and was not offered: {all:?}"
        );
    }
    // **Once each.** `/tools` is in this head's table too, and a list that named it
    // twice would teach it as two verbs.
    let tools = all.iter().filter(|(n, _)| n == "tools").count();
    assert_eq!(tools, 1, "{all:?}");
    // And the head's own hint survives the join.
    let (_, hint) = all.iter().find(|(n, _)| n == "tools").unwrap();
    assert!(!hint.is_empty(), "the head's own row keeps its sentence");

    // Tab now walks the joined list: `/ga` reaches the daemon's `gate`.
    typed(&mut a, "/ga");
    a.key(Key::Tab);
    assert_eq!(a.input(), "/gate");
}

/// **What the fold cannot account for, it does not draw.**
///
/// This is the half that matters more than the folding: a notice whose shape changes under
/// this function must come back **whole** rather than lose the half the parser did not
/// recognise. The monitor notice is the live test of it — it is the same voice, the same
/// bullets and the same closing sentence as a job's, and it is deliberately NOT folded,
/// because leticl does not fold it either and two heads folding different sets is the drift
/// this file has been fixed for before.
#[test]
fn the_notice_fold_refuses_anything_it_cannot_account_for() {
    let cases: &[(&str, &str)] = &[
        ("an ordinary session row", "job j89 exited 0 after 12.4s"),
        (
            "a steering line",
            "steering: you said you would bump the retry budget",
        ),
        (
            "a monitor notice — same bullets, a different kind",
            "[monitor] 1 watch(es) fired:\n  - `w1` (process), declared by you: it ended\nOnly a FIRED watch is an answer about the world.",
        ),
        (
            "a heading with nothing under it",
            "[job] a job you backgrounded has ended:\nThis is the completion arriving on its own — you do not need to wait for it.",
        ),
        (
            "a settlement line whose handle is not in backticks",
            "[job] a job you backgrounded has ended:\n  - j57 exited 0\nThis is the completion arriving on its own — you do not need to wait for it.",
        ),
        (
            "a settlement AFTER the promise",
            "[job] a job you backgrounded has ended:\nThis is the completion arriving on its own — you do not need to wait for it.\n  - `j57` exited 0",
        ),
        (
            "a line that is neither a settlement nor the promise",
            "[job] a job you backgrounded has ended:\n  - `j57` exited 0\nSomething new the daemon has started saying.\nThis is the completion arriving on its own — you do not need to wait for it.",
        ),
    ];
    for (what, text) in cases {
        assert!(
            folded_notice(text, &[]).is_none(),
            "{what} was folded instead of drawn as it arrived: {:?}",
            folded_notice(text, &[])
        );
    }
}

/// **And the row on the glass is the folded line** — the complaint was about what the operator
/// was reading, so the assertion is on a frame and not only on the function.
#[test]
fn a_settled_jobs_row_draws_as_one_line_and_not_as_the_promise() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::TranscriptAppended {
            item_id: "s.0".into(),
            kind: "user".into(),
            ledger_head: String::new(),
        },
    )));
    let notice = "[job] a job you backgrounded has ended:\n  - `j57` exited 0 after 3.0s, wrote 5 bytes: sleep 3; echo done\nThis is the completion arriving on its own — you do not need to wait for it, and `job_wait` would only block you for a result you already have.";
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::TranscriptContent {
            item_id: "s.0".into(),
            item: Box::new(TranscriptItem::User {
                speaker: letibot_transcript::Speaker::Agent,
                parts: vec![letibot_transcript::UserPart::Text {
                    text: notice.into(),
                }],
            }),
        },
    )));
    let screen = a.screen(120, 30).join("\n");
    assert!(
        screen.contains("Job j57 exited 0 after 3.0s, wrote 5 bytes: sleep 3; echo done"),
        "the row names the job, how it ended and what it ran: {screen}"
    );
    assert!(
        !screen.contains("you do not need to wait for it"),
        "R7's promise is to the model, not to the person reading the row: {screen}"
    );
    assert!(
        !screen.contains("a job you backgrounded has ended"),
        "the heading is what a folded line replaces: {screen}"
    );
}

/// **A row this session appended is not drawn as the operator's** — R42, and the
/// operator's own question: *"why job completion events arrive as my messages?"*
///
/// The two rows are the same variant with the same shape and the same text length; the only
/// difference is `speaker`, and the two renderings must differ in the three marks that make
/// the operator's block what it is: **no `▌`**, no raised background, and the faint
/// register. Asserted on the escape sequences for the background and the accent, because
/// the words are identical in both cases — the defect was never the text.
#[test]
fn a_session_row_is_not_drawn_as_the_operators() {
    let mut a = app();
    a.cfg.color = true;
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    for (seq, id, speaker, text) in [
        (
            1u64,
            "s.0",
            letibot_transcript::Speaker::Operator,
            "run the tests",
        ),
        (
            3,
            "s.1",
            letibot_transcript::Speaker::Agent,
            "job j89 exited 0 after 12.4s",
        ),
    ] {
        a.apply(ServerFrame::Event(env(
            seq,
            SessionEvent::TranscriptAppended {
                item_id: id.into(),
                kind: "user".into(),
                ledger_head: String::new(),
            },
        )));
        a.apply(ServerFrame::Event(env(
            seq + 1,
            SessionEvent::TranscriptContent {
                item_id: id.into(),
                item: Box::new(TranscriptItem::User {
                    speaker,
                    parts: vec![letibot_transcript::UserPart::Text { text: text.into() }],
                }),
            },
        )));
    }
    let screen = a.screen(120, 30);
    let theirs = screen
        .iter()
        .find(|l| l.contains("run the tests"))
        .expect("the operator's row is on the screen");
    let ours = screen
        .iter()
        .find(|l| l.contains("job j89 exited 0"))
        .expect("the session's row is on the screen");
    // The mark that means *your own words*, and only on theirs.
    assert!(
        theirs.contains('▌'),
        "the operator's row lost its bar: {theirs:?}"
    );
    assert!(
        !ours.contains('▌'),
        "a job completion is drawn as the operator's own words: {ours:?}"
    );
    // And the label that says whose it is, on ours.
    assert!(
        ours.contains("session ·"),
        "the session's row does not say whose it is: {ours:?}"
    );
    // The raised block is `Role::UserBlock`'s background. Not on ours.
    let block = a.cfg.palette().open(Role::UserBlock);
    assert!(
        theirs.contains(&block),
        "the operator's row lost its block: {theirs:?}"
    );
    assert!(
        !ours.contains(&block),
        "the session's row wears the operator's block: {ours:?}"
    );
    // The faint register, on ours.
    assert!(
        ours.contains("\x1b[2m"),
        "the session's row is not in the register this head keeps for notes about itself: {ours:?}"
    );
}

/// **The jobs count is on the composer's top edge, pluralised, and absent at zero** —
/// R51 item 5, plus the half of it this head has and the brief does not.
///
/// The other head put it on the status row; here the status row was just given to the turn
/// (item 1), and this edge already carries `N subagents running`. Both are facts that stop being
/// drawn when they stop being true, which is what earns a resident edge.
///
/// **The `to a file` clause is the operator's own point, and it is not in leticl's brief:** a
/// job whose output is redirected (R41) has a window that will be empty however long it runs, so
/// **A redirected job's row names the file, because the window cannot.**
///
/// The operator's question, asked while looking at the pane: *"im not sure it lets me to see
/// that in the jobs details, when i 'enter' a job"*. A job whose output went to a file has a
/// capture that is empty BY CONSTRUCTION, so Enter shows nothing however long it runs — and
/// the list is therefore the only place that can say which file to read instead.
#[test]
fn the_jobs_pane_names_the_file_a_redirected_job_writes_to() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(jobs_frame(
        "s",
        vec![letibot_sessionlog::protocol::JobEntry {
            id: "j1".into(),
            command: "cargo test > /tmp/build.log 2>&1".into(),
            // Nobody named it: this test is about the redirect, not the name.
            slug: String::new(),
            how: "asked".into(),
            state: "running".into(),
            running: true,
            never_ran: false,
            redirect: Some("/tmp/build.log".into()),
            produced: 0,
            elapsed_ms: 4_000,
        }],
    ));
    a.key(Key::CtrlQ);
    let screen = a.screen(110, 30).join("\n");
    assert!(
        screen.contains("\u{2192} /tmp/build.log"),
        "the pane must name the file the job writes to:\n{screen}"
    );
    assert!(
        screen.contains("not in the window"),
        "and say why Entering it shows nothing:\n{screen}"
    );
}

/// *"1 job running"* is a truthful count that answers the wrong question. The distinction
/// travels on the wire (`JobEntry::redirect`) because the daemon reads it out of the command.
#[test]
fn the_top_edge_counts_running_jobs_and_says_which_cannot_be_watched() {
    let mut a = app();
    // Attached, because a `Jobs` frame for another session is dropped — the answer is about the
    // session this head is in, and the guard is what stops a switch showing the old session's.
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    let jobs = |cs: Vec<letibot_sessionlog::protocol::JobEntry>| ServerFrame::Jobs {
        session_id: "s".into(),
        jobs: cs,
    };
    let job =
        |id: &str, running: bool, redirect: Option<&str>| letibot_sessionlog::protocol::JobEntry {
            id: id.into(),
            command: "cargo build".into(),
            // The count on the composer's edge is about running jobs, named or not.
            slug: String::new(),
            how: "asked".into(),
            state: if running {
                "running".into()
            } else {
                "exited 0".into()
            },
            running,
            never_ran: false,
            redirect: redirect.map(str::to_string),
            produced: 0,
            elapsed_ms: 10,
        };
    // Zero jobs is no line at all — a count that is always there is furniture.
    a.apply(jobs(vec![job("j1", false, None)]));
    assert_eq!(a.jobs_line(), None, "a settled job is not a running one");
    // One job, pluralised and singular, and the redirect clause only when it applies.
    a.apply(jobs(vec![job("j1", true, None)]));
    assert_eq!(a.jobs_line().as_deref(), Some("1 job running"));
    a.apply(jobs(vec![job("j1", true, None), job("j2", true, None)]));
    assert_eq!(a.jobs_line().as_deref(), Some("2 jobs running"));
    a.apply(jobs(vec![
        job("j1", true, Some("/tmp/build.log")),
        job("j2", true, None),
    ]));
    assert_eq!(
        a.jobs_line().as_deref(),
        Some("2 jobs running · 1 to a file"),
        "the unwatchable one is named before the reader opens the pane"
    );
    // A redirected job that has SETTLED is not counted: the row is about what is running now.
    a.apply(jobs(vec![
        job("j1", true, None),
        job("j2", false, Some("/tmp/build.log")),
    ]));
    assert_eq!(a.jobs_line().as_deref(), Some("1 job running"));
    // **And it is pinned to the top edge, beside the subagent count.** The screen is where the
    // placement is actually decided, so this is asserted on the frame rather than on the string.
    let screen = a.screen(100, 24).join("\n");
    let top = screen
        .lines()
        .find(|l| l.contains('╭'))
        .expect("the composer's top edge is drawn");
    assert!(
        top.contains("1 job running"),
        "the count is not on the top edge: {top:?}"
    );
}

/// **The count is ASKED for when the daemon's table can have changed** — R51 item 5's second
/// half, and the half that makes the row a readout rather than a souvenir.
///
/// There is no `JobStarted` on the wire, so a count folded from events would be a guess. The
/// three moments are the two that say a job started or ended, and the turn boundary, which is
/// the only chance a job that was already running at attach ever gets.
#[test]
fn a_job_starting_ending_or_a_turn_ending_re_reads_the_count() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    // **Drained, and the assertion below depends on it.** An attach asks for the table too (a
    // job already running when this head joined is announced by no event at all), so a
    // leftover `ListJobs` here would make the next assertion pass for the wrong reason.
    assert!(
        a.take_actions().contains(&Action::ListJobs),
        "the attach re-read the count"
    );
    assert!(a.take_actions().is_empty(), "nothing else is owed");
    // 1. A backgrounded call is the job STARTING.
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::ToolFinished {
            turn_id: "t1".into(),
            call_id: "c1".into(),
            outcome: letibot_transcript::ToolOutcome::Backgrounded {
                handle: "j1".into(),
                ran_for_ms: 100,
                how: letibot_transcript::Backgrounding::Asked,
                next: "read it".into(),
            },
            payload_digest: "d".into(),
            inline_bytes: 1,
            full_bytes: 1,
            spill: None,
            repairs: 0,
            edit: None,
        },
    )));
    assert!(
        a.take_actions().contains(&Action::ListJobs),
        "a job started and the count was not re-read"
    );
    // An ordinary `ok` finish starts nothing, so it asks for nothing.
    a.apply(ServerFrame::Event(env(
        3,
        SessionEvent::ToolFinished {
            turn_id: "t1".into(),
            call_id: "c2".into(),
            outcome: letibot_transcript::ToolOutcome::Ok,
            payload_digest: "d".into(),
            inline_bytes: 1,
            full_bytes: 1,
            spill: None,
            repairs: 0,
            edit: None,
        },
    )));
    assert!(
        !a.take_actions().contains(&Action::ListJobs),
        "a call that started nothing re-read the count"
    );
    // 2. A settlement ends one.
    a.apply(ServerFrame::Event(env(
        4,
        SessionEvent::JobSettled {
            job: "j1".into(),
            state: "exited 0".into(),
            produced: 1,
            elapsed_ms: 2,
        },
    )));
    assert!(
        a.take_actions().contains(&Action::ListJobs),
        "a job ended and the count was not re-read"
    );
    // 3. The turn's end, which is the only chance a job running at attach ever gets.
    a.apply(ServerFrame::Event(env(5, testing::turn_finished("t1"))));
    assert!(
        a.take_actions().contains(&Action::ListJobs),
        "the turn ended and the count was not re-read"
    );
}

// -- background jobs -----------------------------------------------------

/// A finished `bash` call that left a job behind: the outcome names the
/// handle, the way the runtime builds it.
/// **A settlement for a job the daemon has not named is not a row.**
///
/// The head used to invent one, with an empty command and an empty `how`,
/// which is how `(command not in this head's window)` reached the operator's
/// screen. The table is the daemon's; an unrecognised settlement waits for
/// the next answer rather than being guessed at.
#[test]
fn a_settlement_folds_onto_a_known_job_and_never_invents_one() {
    let mut a = App::new(plain_cfg(80));
    a.session_id = "s1".into();
    a.apply(jobs_frame(
        "s1",
        vec![daemon_job("j1", "cargo build", true)],
    ));

    // One the daemon never mentioned.
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::JobSettled {
            job: "j99".into(),
            state: "exited 0".into(),
            produced: 1,
            elapsed_ms: 1,
        },
    )));
    assert_eq!(a.jobs.len(), 1, "a settlement invented a row");

    // And one it did: folded, so the pane is live between answers.
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::JobSettled {
            job: "j1".into(),
            state: "killed by job_kill".into(),
            produced: 4096,
            elapsed_ms: 2_500,
        },
    )));
    assert!(!a.jobs[0].running);
    assert_eq!(a.jobs[0].state, "killed by job_kill");
    assert_eq!(a.jobs[0].produced, 4096);
    // The command still reads, because it is the daemon's and was never
    // rebuilt from a turn this head happens to be showing.
    assert_eq!(a.jobs[0].command, "cargo build");
}

#[test]
fn the_jobs_pane_joins_the_command_and_marks_a_running_job() {
    let mut a = App::new(plain_cfg(80));
    a.session_id = "s1".into();
    // Rows come from the daemon, whole. Nothing is joined here.
    a.apply(jobs_frame(
        "s1",
        vec![
            daemon_job("j1", "cargo test --workspace", true),
            daemon_job("j2", "sleep 30", false),
        ],
    ));
    // **Both jobs, so the settled one is unfolded to be drawn** — it lives under the
    // `finished` group by default (the operator's ask), and this test is about the ROW.
    a.jobs_finished_open = true;
    let lines = a.jobs_lines(100).join("\n");
    assert!(lines.contains("cargo test --workspace"), "{lines}");
    assert!(
        lines.contains("[~]"),
        "a running job is marked running: {lines}"
    );
    assert!(
        lines.contains("[x]"),
        "a clean exit is marked done: {lines}"
    );
    assert!(
        !lines.contains("not in this head's window"),
        "the head no longer has a window to be outside of: {lines}"
    );
}

/// **The name the daemon sent survives the fold, and an unnamed row carries nothing.**
///
/// `JobEntry::slug` is the agent's own word for the work (`bash(slug: "release-build")`), and
/// it arrives on the wire already flattened by the daemon — the head neither invents one nor
/// trims one. **The pane's ROW cannot draw it yet**: the layout is rano's and its `JobRow` has
/// no field for a name, which is written at [`crate::ui::panes::jobs`]. So this pins the half
/// that IS this head's — the row the daemon sent is kept whole, name and all — which is what
/// makes the drawing half one field away rather than a re-plumbing.
#[test]
fn a_jobs_name_survives_the_fold_and_an_unnamed_row_carries_none() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(jobs_frame(
        "s",
        vec![
            named_job("j1", "release-build", "cargo build --release", true),
            daemon_job("j2", "cargo test", true),
        ],
    ));
    assert_eq!(a.jobs[0].slug, "release-build");
    assert_eq!(a.jobs[0].id, "j1", "and the handle is untouched");
    assert_eq!(
        a.jobs[1].slug, "",
        "nobody named this one, so there is nothing to draw — not a derivation"
    );
    // And the row it came in on is otherwise exactly what it was.
    assert_eq!(a.jobs[1].command, "cargo test");
}

/// **A REDIRECTED JOB'S OUTPUT PANE NAMES THE FILE** — the operator: *"entering a job never
/// shows me its output - whether it went to file or not"*.
///
/// A redirected job's window is empty BY CONSTRUCTION (R41) — the daemon gave its bytes to
/// the file — so an empty window here must not be described as *it wrote nothing at all*
/// about a job that wrote a build log. The sentence names the file instead.
#[test]
fn the_job_output_pane_names_the_file_a_redirected_job_wrote_to() {
    let mut a = App::new(plain_cfg(100));
    a.session_id = "s1".into();
    a.apply(jobs_frame(
        "s1",
        vec![daemon_job("j9", "cargo build > log", false)],
    ));
    a.jobs[0].redirect = Some("/tmp/build.log".into());
    a.key(Key::CtrlQ);
    // The job is settled: unfold the group, then take its row.
    a.key(Key::Enter);
    a.key(Key::Down);
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::ReadJobOutput {
            job: "j9".into(),
            offset: 0
        })
    );
    assert_eq!(
        a.job_out.as_ref().unwrap().redirect.as_deref(),
        Some("/tmp/build.log"),
        "the pane did not take the row's redirect"
    );
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::JobOutput {
            job: "j9".into(),
            from: 0,
            to: 0,
            produced: 0,
            dropped: 0,
            state: "exited 0".into(),
            never_ran: false,
            lines: Vec::new(),
            next: None,
        },
    )));
    let screen = a.screen(100, 24).join("\n");
    assert!(screen.contains("/tmp/build.log"), "{screen}");
    assert!(
        !screen.contains("it wrote nothing at all."),
        "a job that wrote a build log is described as having written nothing:\n{screen}"
    );
}

/// **Enter on a job row asks the daemon for its output.**
///
/// The operator: *"i cant enter the job to see its output"*. The binding is
/// guarded on an empty composer — Enter with text in it is still a prompt —
/// so both halves are pinned here.
#[test]
fn enter_on_a_job_row_reads_its_output() {
    let mut a = App::new(plain_cfg(100));
    a.session_id = "s1".into();
    a.apply(jobs_frame(
        "s1",
        vec![daemon_job("j7", "cargo test", false)],
    ));
    a.key(Key::CtrlQ);
    assert!(a.jobs_pane);
    // **The job is settled, so it sits under the folded `finished` group**: Enter on the
    // group row (where the cursor starts) unfolds it, and one Down lands on the job.
    a.key(Key::Enter);
    a.key(Key::Down);
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::ReadJobOutput {
            job: "j7".into(),
            offset: 0
        })
    );
    assert!(
        a.jobs_pane,
        "the jobs list stays behind the overlay, so esc returns to it"
    );
    assert!(
        a.job_out.is_some(),
        "the overlay opens at once and says it is reading"
    );
}

#[test]
fn enter_on_a_job_row_asks_the_daemon_for_that_jobs_output() {
    let mut a = App::new(plain_cfg(80));
    a.session_id = "s1".into();
    a.apply(jobs_frame(
        "s1",
        vec![
            daemon_job("j1", "cargo build", true),
            daemon_job("j2", "cargo test", true),
        ],
    ));
    a.key(Key::CtrlQ);
    assert!(a.jobs_pane);
    assert!(
        a.jobs_lines(100).join("\n").contains("\u{25b8}"),
        "a cursor is drawn"
    );
    a.key(Key::Down);
    assert_eq!(a.jobs_sel, 1);
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::ReadJobOutput {
            job: "j2".into(),
            offset: 0
        }),
        "enter reads the selected job, by the daemon's id"
    );
}

/// **§11.6 — a job that never ran is not a job that wrote nothing.**
///
/// The card drew `not run (could not join its scope)` as its header and then
/// `it wrote nothing at all.` under it, which is R17 read backwards: *a row with no
/// output must not look like a row whose output is empty*. Three states, three
/// sentences, and the case chosen by the **daemon's** `never_ran` rather than by
/// `lines.is_empty()` alone.
///
/// The words are asserted literally because the other half of this ruling is a
/// second head — §11.6, *A rules the words; both heads render the same string* — so
/// the literal is the whole of what the two have to agree about. The `JobState`
/// words this head chooses against are pinned on the other side of the wire, in
/// `letibot-tools`' own `every_state_says_whether_a_process_ever_ran`: a reword
/// there breaks that assertion, and a reword here breaks this one.
///
/// A pinned list rather than a property, deliberately: a new state a head has never
/// heard of arrives as an unfamiliar word, falls to the `wrote nothing` arm, and
/// this list is what makes that a visible choice rather than an accident — the same
/// reason the tools test names all five variants.
#[test]
fn a_job_that_never_ran_does_not_read_as_one_that_wrote_nothing() {
    let cases: [(&str, bool, &str); 6] = [
        (
            "running",
            false,
            "it is running and has written nothing yet.",
        ),
        ("exited 0", false, "it wrote nothing at all."),
        ("exited 1", false, "it wrote nothing at all."),
        ("signalled 9", false, "it wrote nothing at all."),
        ("killed by job_kill", false, "it wrote nothing at all."),
        (
            "not run (could not join its scope)",
            true,
            "it never ran, so there is nothing it could have written.",
        ),
    ];
    for (state, never_ran, want) in cases {
        let mut a = App::new(plain_cfg(100));
        a.session_id = "s1".into();
        a.apply(jobs_frame(
            "s1",
            vec![daemon_job("j3", "cargo build", false)],
        ));
        a.key(Key::CtrlQ);
        // Unfold the `finished` group, then take the job row — see the sibling tests.
        a.key(Key::Enter);
        a.key(Key::Down);
        a.key(Key::Enter);
        a.apply(ServerFrame::Event(env(
            1,
            SessionEvent::JobOutput {
                job: "j3".into(),
                from: 0,
                to: 0,
                produced: 0,
                dropped: 0,
                state: state.into(),
                never_ran,
                lines: Vec::new(),
                next: None,
            },
        )));
        let drawn = a.screen(100, 24).join("\n");
        assert!(drawn.contains(want), "{state}: want {want:?} in\n{drawn}");
        if never_ran {
            assert!(
                !drawn.contains("wrote nothing"),
                "{state}: the contradiction is back — a window described as one that \
                     wrote nothing, under a header saying it never started:\n{drawn}"
            );
        }
    }
}

/// **The window lands in the pane.** The `JobOutput` event the daemon publishes
/// for a `ReadJobOutput` fills the overlay the jobs pane opened, offsets and
/// all, and the overlay pages with → and ← without ever touching the
/// conversation.
#[test]
fn the_job_output_event_fills_the_overlay_and_it_pages() {
    let mut a = App::new(plain_cfg(100));
    a.session_id = "s1".into();
    a.apply(jobs_frame(
        "s1",
        vec![daemon_job("j3", "cargo build", false)],
    ));
    a.key(Key::CtrlQ);
    // Unfold the `finished` group, then take the job row.
    a.key(Key::Enter);
    a.key(Key::Down);
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::ReadJobOutput {
            job: "j3".into(),
            offset: 0
        })
    );
    // The first page arrives: the head is told where it is and where next is.
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::JobOutput {
            job: "j3".into(),
            from: 0,
            to: 10,
            produced: 30,
            dropped: 0,
            state: "exited 0".into(),
            never_ran: false,
            lines: vec!["line one".into(), "line two".into()],
            next: Some(10),
        },
    )));
    let v = a.job_out.as_ref().expect("the overlay is open");
    assert!(!v.loading, "the answer clears the wait");
    assert_eq!(v.state, "exited 0");
    assert_eq!(v.lines.len(), 2);
    assert_eq!(v.next, Some(10));
    let screen = a.screen(100, 24).join("\n");
    assert!(screen.contains("job output — j3"), "{screen}");
    assert!(
        screen.contains("exited 0 — bytes 0..10 of 30"),
        "the pane draws the offsets it was sent, not a parsed sentence: {screen}"
    );

    // → asks for the page the daemon named, and remembers where it was.
    assert_eq!(
        a.key(Key::Right),
        Some(Action::ReadJobOutput {
            job: "j3".into(),
            offset: 10
        })
    );
    assert_eq!(a.job_out.as_ref().unwrap().back, vec![0]);
    // ← walks back the way → came, by the offset it was given — the head never
    // computes a page size, because the size is the daemon's.
    assert_eq!(
        a.key(Key::Left),
        Some(Action::ReadJobOutput {
            job: "j3".into(),
            offset: 0
        })
    );
    assert!(a.job_out.as_ref().unwrap().back.is_empty());
    // At the front there is no page before the first byte, so ← does nothing.
    assert_eq!(a.key(Key::Left), None);
    // Esc returns to the jobs list, which never closed.
    a.key(Key::Esc);
    assert!(a.job_out.is_none());
    assert!(a.jobs_pane, "esc goes back to jobs, not out of everything");
}

/// **A refused read is answered in the pane, not left at `reading…`.** A job can
/// fall out of the host's table between the listing and Enter — the daemon then
/// answers with a `job_output_refused` warning, and the pane that asked must say
/// so rather than wait for a window that is not coming.
#[test]
fn a_refused_job_output_read_lands_in_the_pane() {
    let mut a = App::new(plain_cfg(100));
    a.session_id = "s1".into();
    a.apply(jobs_frame(
        "s1",
        vec![daemon_job("j4", "cargo build", false)],
    ));
    a.key(Key::CtrlQ);
    // Unfold the `finished` group, then take the job row.
    a.key(Key::Enter);
    a.key(Key::Down);
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::ReadJobOutput {
            job: "j4".into(),
            offset: 0
        })
    );
    assert!(a.job_out.as_ref().unwrap().loading);
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::Warning {
            code: "job_output_refused".into(),
            detail: "no job `j4` here; `/job` with no argument lists them".into(),

            compaction: None,
        },
    )));
    let v = a.job_out.as_ref().expect("the pane is still open");
    assert!(!v.loading, "the refusal ends the wait");
    assert_eq!(
        v.error.as_deref(),
        Some("no job `j4` here; `/job` with no argument lists them")
    );
    let screen = a.screen(100, 24).join("\n");
    assert!(screen.contains("the daemon refused this read"), "{screen}");
    assert!(screen.contains("no job `j4` here"), "{screen}");
}

/// Jobs belong to the session that started them. Carried across a switch the
/// pane drew the old session's ids and byte counts, and Enter on one of those
/// rows now asks the NEW session for a job that was never here. The tree next
/// door is dropped on a switch for the same reason; so is this.
#[test]
fn switching_drops_the_old_sessions_jobs_and_coming_back_rebuilds_them() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "parent", true)],
        Hub::new("s").snapshot(),
    ));
    a.apply(jobs_frame(
        "s",
        vec![daemon_job("j1", "cargo build", false)],
    ));
    assert_eq!(a.jobs.len(), 1);
    a.jobs_sel = 0;

    a.apply(hello(
        "s2",
        vec![brief("s", "parent", true)],
        Hub::new("s2").snapshot(),
    ));
    assert_eq!(a.session_id, "s2");
    assert!(a.jobs.is_empty(), "the other session's jobs came along");
    assert_eq!(a.jobs_sel, 0);
    // And the pane asks the new session's daemon rather than drawing a list
    // it built for the old one.
    assert_eq!(a.key(Key::CtrlQ), Some(Action::ListJobs));
    let screen = a.screen(100, 24).join("\n");
    assert!(screen.contains("none."), "{screen}");
    a.key(Key::Esc);

    // A jobs frame for a session this head is not in is ignored.
    a.apply(jobs_frame("s", vec![daemon_job("j9", "old", false)]));
    assert!(a.jobs.is_empty(), "another session's table was folded in");
}
