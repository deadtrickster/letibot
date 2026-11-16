//! A change detected after a command draws its diff: `tools::detect` names what changed and
//! materialises both sides, and the head draws it. The test drives the real tool because the loss
//! was in the seam, so a fixture on either side of it would have hidden it.

use super::*;

/// `git` in the fixture tree, failing the test rather than the call. The sweep is a `git status`
/// and nothing else, so outside a repository it has nothing to compare - the fixture has to be one.
fn git(root: &std::path::Path, args: &[&str]) {
    let out = letibot_tools::testing::git(root)
        .args(args)
        .output()
        .expect("git");
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

/// The operator's own shape, end to end. A `python3` heredoc rewrites `X.md`; the tool
/// detects it; the head draws the two sides; and the note beside them still says the change
/// was detected after the command rather than made by `edit`.
#[test]
fn a_python_edit_detected_after_a_command_draws_its_diff() {
    let Some(_) = letibot_tokencore::apparatus::present(
        "a process-lifetime tree (cgroup v2; process groups on macOS)",
        letibot_tools::host_tree().is_ok(),
    ) else {
        return;
    };
    let mut h = letibot_tools::testing::runner_harness()
        .expect("the gate above said this box has a process-lifetime tree");
    let root = h.root().to_path_buf();
    git(&root, &["init", "-q"]);
    h.write_file("X.md", "one\ntwo\n");
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-qm", "first"]);

    // The operator's shape, with `chr(10)` for the newline so the fixture carries no
    // escaping a reader has to unroll before they can see what the command does.
    let cmd = "python3 - <<'PYEOF'\n\
               p = 'X.md'\n\
               s = open(p).read()\n\
               open(p, 'w').write(s + 'three' + chr(10))\n\
               PYEOF\n";
    let r = h.call("bash", &serde_json::json!({ "command": cmd }).to_string());

    // The tool's half first: it saw the change, it holds both sides, and the label it puts
    // beside them says where they came from. Asserted here so that a failure below is known
    // to be the head's and not this.
    assert_eq!(
        r.edits.first().map(|e| e.path.as_str()),
        Some("X.md"),
        "the sweep did not report the change: {}",
        r.render()
    );
    assert!(
        r.render().contains("detected afterwards"),
        "the note that says HOW the pair was obtained: {}",
        r.render()
    );

    // The event the runtime emitted, which is what a head watching live is told.
    let at = h
        .sink
        .events
        .iter()
        .position(|e| matches!(e, letibot_tools::ToolEvent::Finished { .. }))
        .expect("the call finished");
    assert!(
        matches!(
            &h.sink.events[at],
            letibot_tools::ToolEvent::Finished { edits, .. } if !edits.is_empty()
        ),
        "the pair has to leave the tool on the event: {:?}",
        h.sink.events[at]
    );
    let lifted = letibot_sessionlog::lift_tools::from_tool_event(h.sink.events.remove(at));
    let row = letibot_tools::runtime::ToolRuntime::transcript_item(&r);

    // …and now the head, through the same frames the daemon sends: the proposal, the
    // finish, the row announced, the row's body.
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Event(env_at(
        1,
        1_000,
        testing::turn_started("t1"),
    )));
    a.apply(ServerFrame::Event(env_at(
        2,
        1_000,
        testing::proposed_on("t1", &r.call_id, "bash", "python3 - <<'PYEOF' …"),
    )));
    a.apply(ServerFrame::Event(env_at(3, 2_000, lifted)));

    // The card names the file before the row's body lands, folded like the row will be: a change
    // that appeared only at the hand-over would be a long command finishing with nothing to read.
    let live = a.screen(120, 40).join("\n");
    assert!(
        live.contains("X.md · +") && live.contains("/t unfolds it"),
        "the live card, before the row's body lands:\n{live}"
    );

    a.apply(ServerFrame::Event(env_at(
        4,
        3_000,
        testing::appended("s.1", "tool_result"),
    )));
    a.record_item("s.1", row);

    // Folded, which is the row the operator was looking at: the note says how the change was
    // obtained, once, and the file is one line naming what `/t` opens.
    assert_eq!(
        a.tools,
        Fold::Folded,
        "the default this case is measured at"
    );
    let folded = a.screen(120, 40).join("\n");
    assert!(
        folded.contains("X.md · +") && folded.contains("/t unfolds it"),
        "the file is named with the way to its diff:\n{folded}"
    );
    assert!(
        !folded.contains("+ three"),
        "folded, the diff is not drawn:\n{folded}"
    );
    assert_eq!(
        folded.matches("detected afterwards").count(),
        1,
        "how it was obtained is said once:\n{folded}"
    );

    // …and open, where all of it is on the screen and nothing is elided.
    a.tools = Fold::Open;
    a.refold();
    let open = a.screen(120, 40).join("\n");
    assert!(
        open.contains("+ three"),
        "unfolded, the whole diff:\n{open}"
    );
    assert!(
        open.contains("@@ -1,2 +1,3 @@"),
        "the hunk a diff draws and nothing else does:\n{open}"
    );
    assert_eq!(
        open.matches("detected afterwards").count(),
        1,
        "and still said once:\n{open}"
    );
}

/// Folded, a detected diff is one line with the count of what `/t` opens; open, it is drawn whole.
/// No provenance line is drawn under it: the row's first note says that, once.
#[test]
fn a_detected_diff_is_folded_to_one_line_until_t() {
    let after: String = (1..=24)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let e = letibot_transcript::ToolEditExcerpt {
        path: "X.md".into(),
        created: true,
        before_start: 1,
        after_start: 1,
        before_lines: 0,
        after_lines: 24,
        truncated: false,
        before: String::new(),
        after,
    };
    let cfg = plain_cfg(120);
    let target = "python3 - <<'PYEOF' …";

    let rows = |fold: Fold| {
        crate::ui::detected_diff_rows(
            std::slice::from_ref(&e),
            crate::ui::transcript::call::DiffRows {
                target,
                width: 116,
                cfg: &cfg,
                diff_split: true,
                fold,
                under_a_panel: false,
            },
        )
    };
    let folded = rows(Fold::Folded);
    assert_eq!(folded.len(), 1, "{folded:?}");
    assert!(
        folded[0].contains("X.md · +") && folded[0].contains("diff rows · /t unfolds it"),
        "the count of what the fold hid, and the way to it: {folded:?}"
    );

    let open = rows(Fold::Open);
    assert!(
        !open.iter().any(|r| r.contains("diff rows")),
        "nothing is elided when it is open: {open:?}"
    );
    assert!(
        open.iter().any(|r| r.contains("line 1")) && open.iter().any(|r| r.contains("line 24")),
        "the whole change is on the screen: {open:?}"
    );
    assert!(
        !open.iter().any(|r| r.contains("detected afterwards")),
        "no footer repeats the note: {open:?}"
    );
}

/// Two files changed by one command are two diffs on one row: one line each while folded, drawn
/// whole when the fold is open, so a sweep that rewrote twenty files does not bury its command.
#[test]
fn two_changed_files_are_two_diffs_and_the_count_is_said() {
    let one = |path: &str| letibot_transcript::ToolEditExcerpt {
        path: path.into(),
        created: false,
        before_start: 1,
        after_start: 1,
        before_lines: 3,
        after_lines: 3,
        truncated: false,
        before: "one\ntwo\nthree".into(),
        after: "one\nTWO\nthree".into(),
    };
    let edits = [one("a.txt"), one("b.txt")];
    let cfg = plain_cfg(120);

    let rows = |edits: &[letibot_transcript::ToolEditExcerpt], fold: Fold| {
        crate::ui::detected_diff_rows(
            edits,
            crate::ui::transcript::call::DiffRows {
                target: "python3 -c …",
                width: 116,
                cfg: &cfg,
                diff_split: true,
                fold,
                under_a_panel: false,
            },
        )
    };
    let folded = rows(&edits, Fold::Folded);
    assert_eq!(folded.len(), 2, "one line per file: {folded:?}");
    assert!(
        folded[0].contains("a.txt · +") && folded[1].contains("b.txt · +"),
        "each named with what it hides: {folded:?}"
    );
    assert!(
        !folded.iter().any(|r| r.contains("TWO")),
        "folded, no diff is drawn: {folded:?}"
    );

    // And unfolded, each file's diff is drawn.
    let open = rows(&edits, Fold::Open);
    let both = open.join("\n");
    assert!(
        both.contains("a.txt") && both.contains("b.txt"),
        "both files are on the screen: {both}"
    );
    assert_eq!(
        both.matches("TWO").count(),
        2,
        "the change itself is drawn for each file: {both}"
    );
}

/// A bash row as the runtime hands it over: the payload with its notes first, and its excerpts.
fn bash_row(payload: &str, edits: Vec<letibot_transcript::ToolEditExcerpt>) -> App {
    let hub = Hub::new("s");
    hub.publish(testing::appended("s.1", "tool_result"));
    hub.record_item(
        "s.1",
        TranscriptItem::ToolResult {
            call_id: "c1".into(),
            name: "bash".into(),
            outcome: letibot_transcript::ToolOutcome::Ok,
            payload: payload.into(),
            edits,
            origin: None,
            media: None,
            ran_ms: None,
            decision: None,
        },
    );
    let mut a = app();
    a.apply(hello("s", vec![brief("s", "one", false)], hub.snapshot()));
    a
}

fn excerpt(path: &str) -> letibot_transcript::ToolEditExcerpt {
    letibot_transcript::ToolEditExcerpt {
        path: path.into(),
        created: false,
        before_start: 1,
        after_start: 1,
        before_lines: 2,
        after_lines: 2,
        truncated: false,
        before: "one\nedited".into(),
        after: "one\ntwo".into(),
    }
}

/// `git checkout other && git checkout -` that leaves the tree as it was: one line, no diff.
#[test]
fn a_checkout_there_and_back_draws_one_line_and_no_diff() {
    let mut a = bash_row(
        "[note] git: checkout ×2 · 9 files moved · net change: none\nSwitched to branch 'ik/x'\n",
        Vec::new(),
    );
    let screen = a.screen(120, 40).join("\n");
    assert_eq!(
        screen.matches("net change: none").count(),
        1,
        "the git line, once:\n{screen}"
    );
    assert!(!screen.contains("diff rows"), "no diff:\n{screen}");
    assert!(
        !screen.contains("detected afterwards"),
        "no provenance footer:\n{screen}"
    );
}

/// `sed -i` on two files: the note once, and two folded diffs.
#[test]
fn a_sed_on_two_files_draws_one_note_and_two_folded_diffs() {
    let mut a = bash_row(
        "[note] this command changed 2 files - diffs detected afterwards, not made by `edit`\n\
         a.txt, b.txt\n",
        vec![excerpt("a.txt"), excerpt("b.txt")],
    );
    let screen = a.screen(120, 40).join("\n");
    assert_eq!(
        screen.matches("detected afterwards").count(),
        1,
        "said once:\n{screen}"
    );
    assert_eq!(
        screen.matches("diff rows · /t unfolds it").count(),
        2,
        "two folded diffs:\n{screen}"
    );
    assert!(!screen.contains("+ two"), "folded, not drawn:\n{screen}");
    a.tools = Fold::Open;
    a.refold();
    let open = a.screen(120, 40).join("\n");
    assert!(open.contains("a.txt") && open.contains("b.txt"), "{open}");
    assert!(!open.contains("diff rows"), "open draws them:\n{open}");
}

/// A `git stash` that leaves the tree changed: the git line, and the net-changed file folded.
#[test]
fn a_stash_that_leaves_the_tree_changed_draws_the_git_line_and_folds_the_net_change() {
    let mut a = bash_row(
        "[note] git: stash · 1 file moved · net change: 1 file\na.txt\nSaved working directory\n",
        vec![excerpt("a.txt")],
    );
    let screen = a.screen(120, 40).join("\n");
    assert!(
        screen.contains("git: stash · 1 file moved · net change: 1 file"),
        "{screen}"
    );
    assert_eq!(
        screen.matches("a.txt · +").count(),
        1,
        "the net change, folded:\n{screen}"
    );
    assert!(!screen.contains("detected afterwards"), "{screen}");
}
