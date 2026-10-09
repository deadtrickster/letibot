//! **A change detected after a command draws its diff.**
//!
//! `crates/tools/src/detect.rs` exists for the operator's own words — *"how to catch edits
//! via python or git merges"* — and its commit is titled *"an edit made by a shell still
//! draws a diff"*. It names what changed, materialises the two sides for the one file a card
//! is drawn for, and puts them on the `ToolFinished` event and on the transcript row, with a
//! note that keeps saying **how** they were obtained rather than claiming the model called
//! `edit`.
//!
//! What it does not do is draw anything: it is the tool half. And the head's half was
//! missing, which the operator found by watching a `python3` heredoc rewrite a file and
//! getting the note, the fold's `+14 lines`, the `ctrl-v` — and no diff. Their report is the
//! whole of it: *"right, but i didnt see the diff"*, and then *"we had this explicit feature
//! to detect python edits"*.
//!
//! # Why this test drives the real tool
//!
//! The loss was in the seam, so a fixture on either side of it would have hidden it. A
//! hand-built excerpt would have asserted that the head draws a diff for a row this test
//! wrote; the question is whether it draws one for the row the tool actually hands it, whose
//! name is `bash` and whose payload is a command's output. So: a real `bash` call, a real
//! `python3` heredoc, the real event the runtime emitted, and the real row the runtime builds
//! — and the assertion is about the screen.

use super::*;

/// `git` in the fixture tree, failing the test rather than the call.
///
/// The sweep is a `git status` and nothing else: outside a repository it has nothing to
/// compare, so the fixture has to be one.
fn git(root: &std::path::Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .expect("git");
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

/// **The operator's own shape, end to end.** A `python3` heredoc rewrites `X.md`; the tool
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
    git(&root, &["config", "user.email", "t@t"]);
    git(&root, &["config", "user.name", "t"]);
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
        r.edit.as_ref().map(|e| e.path.as_str()),
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
            letibot_tools::ToolEvent::Finished { edit: Some(_), .. }
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

    // **The card draws it too, before the row's body lands.** A change that appeared only at
    // the hand-over would be the operator watching a long command finish with nothing to read
    // and then watching the diff arrive with the row — the same flicker, at the same seam,
    // that the settled row's own note has already cost once.
    let live = a.screen(120, 40).join("\n");
    assert!(
        live.contains("+ three"),
        "the live card, before the row's body lands:\n{live}"
    );

    a.apply(ServerFrame::Event(env_at(
        4,
        3_000,
        testing::appended("s.1", "tool_result"),
    )));
    a.record_item("s.1", row);

    // **Folded, which is the row the operator was looking at.** Their line was `▸`, and a
    // diff that appears only once you unfold a row you did not know held one is the same no
    // diff — so the panel is drawn folded too, exactly as a real edit's is.
    assert_eq!(
        a.tools,
        Fold::Folded,
        "the default this case is measured at"
    );
    let folded = a.screen(120, 40).join("\n");
    assert!(
        folded.contains("@@ -1,2 +1,3 @@"),
        "the hunk a diff draws and nothing else does:\n{folded}"
    );
    assert!(
        folded.contains("+ three"),
        "the change must be DRAWN, not only named:\n{folded}"
    );
    assert!(
        folded.contains("detected afterwards"),
        "and the label must keep saying how it was obtained:\n{folded}"
    );

    // …and open, where all of it is on the screen and nothing is elided.
    a.tools = Fold::Open;
    let open = a.screen(120, 40).join("\n");
    assert!(
        open.contains("+ three"),
        "unfolded, the whole diff:\n{open}"
    );
}

/// **The panel folds the way a real edit's does**, which is the half of "however the head
/// shows a real edit's diff" that a three-line change cannot exercise: folded keeps the first
/// eight rows so the change is on the screen without unfolding anything, and the count of what
/// is left is printed beside the chord that opens the rest — an elision without its
/// denominator is a silent cut.
///
/// No process here: this is [`crate::ui::detected_diff_rows`]'s own rule, and it is asserted
/// on the rows rather than on a screen because the screen-level case is the test above.
#[test]
fn a_detected_panel_folds_the_way_an_edits_does() {
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

    let folded = crate::ui::detected_diff_rows(&e, target, 116, &cfg, true, Fold::Folded);
    assert!(
        folded
            .iter()
            .any(|r| r.contains("diff rows · /t unfolds it")),
        "the count of what the fold hid, and the way to it: {folded:?}"
    );
    assert!(
        folded.iter().any(|r| r.contains("line 1")),
        "the change is on the screen while the row is folded: {folded:?}"
    );
    assert!(
        !folded.iter().any(|r| r.contains("line 24")),
        "the tail is what the count above stands for: {folded:?}"
    );

    let open = crate::ui::detected_diff_rows(&e, target, 116, &cfg, true, Fold::Open);
    assert!(
        open.len() > folded.len(),
        "unfolded draws the rest of it: {open:?}"
    );
    assert!(
        !open.iter().any(|r| r.contains("diff rows")),
        "nothing is elided when it is open: {open:?}"
    );
    assert!(
        open.iter().any(|r| r.contains("line 24")),
        "the last row of the change is on the screen: {open:?}"
    );
}
