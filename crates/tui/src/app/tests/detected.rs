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
        r.render().contains("by the command, not `edit`"),
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

    // The card carries the change before the row's body lands, drawn as an edit card draws its
    // own: a change that appeared only at the hand-over would be a long command finishing with
    // nothing to read.
    let live = a.screen(120, 40).join("\n");
    assert!(
        live.contains("X.md") && live.contains("+ three"),
        "the live card, before the row's body lands:\n{live}"
    );

    a.apply(ServerFrame::Event(env_at(
        4,
        3_000,
        testing::appended("s.1", "tool_result"),
    )));
    a.record_item("s.1", row);

    // Folded, which is the row the operator was looking at: the diff's own rows, as an edit card
    // shows them, and the note that says how the change was obtained - once.
    assert_eq!(
        a.tools,
        Fold::Folded,
        "the default this case is measured at"
    );
    let folded = a.screen(120, 40).join("\n");
    assert!(
        folded.contains("X.md") && folded.contains("@@ -1,2 +1,3 @@"),
        "the diff's own name and hunk:\n{folded}"
    );
    assert!(
        folded.contains("+ three"),
        "folded keeps the change itself on the screen:\n{folded}"
    );
    assert_eq!(
        folded.matches("by the command, not `edit`").count(),
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
        open.matches("by the command, not `edit`").count(),
        1,
        "and still said once:\n{open}"
    );
}

/// A detected diff is drawn the way an edit card draws its own: folded, the change's first rows
/// and the count of what `/t` opens; open, all of them. The one summary line this replaced said a
/// diff existed and showed none of it, so a python edit and an `edit` call read as two things.
#[test]
fn a_detected_diff_is_drawn_as_the_edit_card_draws_one() {
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
                // Unified, so a row's text is its own and not padded to the panel's width.
                diff_split: false,
                fold,
                under_a_panel: false,
            },
        )
    };
    let folded = rows(Fold::Folded);
    // The rows are the DIFF's own rows: the change under its hunk, indented as a card indents
    // them, not a second rendering of the excerpt.
    let all = crate::ui::edit_diff(&e, 116, &cfg, false).rows_under(target);
    let drawn: Vec<String> = all.iter().map(|l| l.plain()).collect();
    assert!(
        folded.len() > 1 && folded.len() < drawn.len(),
        "folded keeps some of the diff and not all of it: {} rows of {}: {folded:?}",
        folded.len(),
        drawn.len()
    );
    let kept = &folded[..folded.len() - 1];
    for (got, want) in kept.iter().zip(&drawn) {
        assert_eq!(
            got.trim_end(),
            format!("  {}", want.trim_end()),
            "the diff's own row, folded"
        );
    }
    assert_eq!(
        kept.len(),
        8,
        "the card's own fold keeps the first rows: {kept:?}"
    );
    // The edit card's own seam, counting what it hid.
    let seam = format!("… +{} diff rows · /t unfolds it", drawn.len() - 8);
    assert_eq!(
        folded.last().map(|l| l.trim()),
        Some(seam.as_str()),
        "the rows after the kept ones are counted: {folded:?}"
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

/// Two files changed by one command are two diffs on one row, each drawn as an edit card draws
/// its own, so a sweep that rewrote twenty files does not bury its command.
#[test]
fn two_changed_files_are_two_diffs_each_drawn_as_an_edit_card() {
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
    // Each file is its own block: a name line and the rows of its change, folded or open.
    let folded = rows(&edits, Fold::Folded);
    let both = folded.join("\n");
    assert!(
        folded[0].contains("a.txt") && folded.iter().any(|r| r.contains("b.txt")),
        "each file names itself, since the header names the command: {folded:?}"
    );
    assert_eq!(
        both.matches("TWO").count(),
        2,
        "the change itself is drawn for each file: {both}"
    );

    // And unfolded, each file's diff is drawn whole.
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
    assert!(
        !open.iter().any(|r| r.contains("diff rows")),
        "nothing is elided when it is open: {open:?}"
    );
}

/// A tool result as the runtime hands it over.
fn result_item(
    name: &str,
    payload: &str,
    edits: Vec<letibot_transcript::ToolEditExcerpt>,
) -> TranscriptItem {
    TranscriptItem::ToolResult {
        call_id: "c1".into(),
        name: name.into(),
        outcome: letibot_transcript::ToolOutcome::Ok,
        payload: payload.into(),
        edits,
        origin: None,
        media: None,
        ran_ms: None,
        decision: None,
    }
}

/// A bash row as the runtime hands it over: the payload with its notes first, and its excerpts.
fn bash_row(payload: &str, edits: Vec<letibot_transcript::ToolEditExcerpt>) -> App {
    let hub = Hub::new("s");
    hub.publish(testing::appended("s.1", "tool_result"));
    hub.record_item("s.1", result_item("bash", payload, edits));
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
        !screen.contains("by the command, not `edit`"),
        "a sweep that netted nothing says only its git line:\n{screen}"
    );
}

/// `sed -i` on two files: the note once, and two diffs drawn as edit cards.
#[test]
fn a_sed_on_two_files_draws_one_note_and_two_diffs() {
    let mut a = bash_row(
        "[note] this command changed 2 files - by the command, not `edit`\na.txt, b.txt\n",
        vec![excerpt("a.txt"), excerpt("b.txt")],
    );
    let screen = a.screen(120, 40).join("\n");
    assert_eq!(
        screen.matches("by the command, not `edit`").count(),
        1,
        "said once:\n{screen}"
    );
    assert_eq!(
        screen.matches("@@ -1,2 +1,2 @@").count(),
        2,
        "both files' hunks are drawn:\n{screen}"
    );
    assert_eq!(
        screen.matches("+ two").count(),
        2,
        "and both changes:\n{screen}"
    );
    a.tools = Fold::Open;
    a.refold();
    let open = a.screen(120, 40).join("\n");
    assert!(open.contains("a.txt") && open.contains("b.txt"), "{open}");
    assert!(!open.contains("diff rows"), "open draws them:\n{open}");
}

/// A `git stash` that leaves the tree changed: the git line, and the net-changed file's diff.
#[test]
fn a_stash_that_leaves_the_tree_changed_draws_the_git_line_and_the_net_change() {
    let mut a = bash_row(
        "[note] git: stash · 1 file moved · net change: 1 file\na.txt\nSaved working directory\n",
        vec![excerpt("a.txt")],
    );
    let screen = a.screen(120, 40).join("\n");
    assert!(
        screen.contains("git: stash · 1 file moved · net change: 1 file"),
        "{screen}"
    );
    assert!(
        screen.contains("@@ -1,2 +1,2 @@") && screen.contains("+ two"),
        "the net change, drawn as a card draws it:\n{screen}"
    );
    assert!(!screen.contains("by the command, not `edit`"), "{screen}");
}
