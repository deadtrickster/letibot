use super::*;

// What a command changed, and what the tool attaches for it. No process host is needed here
// (this box has no cgroup), so the command is run by the test itself and the sweep is what is
// measured: the same file the same way `bash` would have written it, then the attach.

/// `git` with this box's own config held out of it: the fixtures must commit the same way on
/// every machine, and `testing::git` is behind a feature the unit tests are not built with.
fn git(root: &std::path::Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .current_dir(root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", "letibot fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@letibot.invalid")
        .env("GIT_COMMITTER_NAME", "letibot fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@letibot.invalid")
        .args(args)
        .output()
        .expect("git");
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

/// A repository with two committed files, and a workspace that is its root.
fn repo(name: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("letibot-sweep-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("fuzz")).expect("mkdir");
    std::fs::write(root.join("a.txt"), "one\ntwo\n").expect("write");
    std::fs::write(root.join("fuzz/list.inc"), "alpha\nbeta\n").expect("write");
    git(&root, &["init", "-q"]);
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-qm", name]);
    root
}

fn sweep_of(root: &std::path::Path) -> Sweep {
    Sweep::before(root.to_path_buf(), Some(root.to_path_buf()))
}

/// Attach over a sweep and say what the row now holds.
fn attached(
    sweep: &Sweep,
    files: &crate::files::FileLedger,
) -> (Invocation, Vec<(String, Vec<u8>)>) {
    let wrote: std::sync::Mutex<Vec<(String, Vec<u8>)>> = std::sync::Mutex::new(Vec::new());
    let mut inv = Invocation::ok("ran");
    attach_changes(
        &mut inv,
        sweep,
        files,
        crate::runtime::WritesWith {
            edit: true,
            write: true,
        },
        &|path, bytes| {
            wrote
                .lock()
                .unwrap()
                .push((path.to_string(), bytes.to_vec()))
        },
    );
    let wrote = wrote.into_inner().unwrap();
    (inv, wrote)
}

/// The editor's shape: an inline python program that reads a tracked file and writes it back.
/// The command is the real one, run here rather than through the boundary this box lacks.
#[test]
fn a_python_inline_program_that_rewrites_a_tracked_file_draws_its_diff() {
    let root = repo("python");
    let sweep = sweep_of(&root);
    let out = std::process::Command::new("python3")
        .current_dir(&root)
        .args([
            "-c",
            "p = 'fuzz/list.inc'; s = open(p).read(); open(p, 'w').write(s + 'gamma' + chr(10))",
        ])
        .output()
        .expect("python3");
    assert!(out.status.success(), "{out:?}");

    let files = crate::files::FileLedger::new();
    let (inv, wrote) = attached(&sweep, &files);
    let edit = inv.edits.first().expect("a diff");
    assert_eq!(edit.path, "fuzz/list.inc");
    assert_eq!(edit.before, "alpha\nbeta\n");
    assert_eq!(edit.after, "alpha\nbeta\ngamma\n");
    assert!(!edit.created);
    assert!(
        inv.notes.iter().any(|n| n.contains("detected afterwards")),
        "{:?}",
        inv.notes
    );
    // The ledger and the provenance sink both learned whose bytes these are.
    assert_eq!(
        files.whole_of("fuzz/list.inc"),
        Some((
            crate::spill::content_hash(b"alpha\nbeta\ngamma\n"),
            "alpha\nbeta\ngamma\n".len()
        ))
    );
    assert_eq!(wrote.len(), 1, "{wrote:?}");
    assert_eq!(wrote[0].0, "fuzz/list.inc");
    let _ = std::fs::remove_dir_all(&root);
}

/// The operator's arango shape, with the count it really had: one `python3 -c` that rewrites two
/// tracked files is two diffs on one row, not a name and an apology (R161).
#[test]
fn a_python_inline_program_that_rewrites_two_files_draws_both_diffs() {
    let root = repo("python-two");
    let sweep = sweep_of(&root);
    let out = std::process::Command::new("python3")
        .current_dir(&root)
        .args([
            "-c",
            "for p in ('a.txt', 'fuzz/list.inc'):\n    s = open(p).read()\n    \
                 open(p, 'w').write(s + 'gamma' + chr(10))",
        ])
        .output()
        .expect("python3");
    assert!(out.status.success(), "{out:?}");

    let (inv, wrote) = attached(&sweep, &crate::files::FileLedger::new());
    let paths: Vec<&str> = inv.edits.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(
        paths,
        vec!["a.txt", "fuzz/list.inc"],
        "one excerpt per file"
    );
    assert_eq!(inv.edits[0].before, "one\ntwo\n");
    assert_eq!(inv.edits[0].after, "one\ntwo\ngamma\n");
    assert_eq!(inv.edits[1].after, "alpha\nbeta\ngamma\n");
    // The ledger and the provenance sink learn both files, not only the one a card names.
    assert_eq!(wrote.len(), 2, "{wrote:?}");
    let note = inv.notes.join("\n");
    assert!(note.contains("changed 2 files"), "{note}");
    assert!(note.contains("detected afterwards"), "{note}");
    let _ = std::fs::remove_dir_all(&root);
}

/// `sed -i` is the other shape a model reaches for.
#[test]
fn a_sed_in_place_rewrite_draws_its_diff() {
    let root = repo("sed");
    let sweep = sweep_of(&root);
    let out = std::process::Command::new("sed")
        .current_dir(&root)
        // BSD sed takes the backup suffix as its own argument, GNU sed takes none.
        .args(if cfg!(target_os = "macos") {
            &["-i", "", "s/two/TWO/", "a.txt"][..]
        } else {
            &["-i", "s/two/TWO/", "a.txt"][..]
        })
        .output()
        .expect("sed");
    assert!(out.status.success(), "{out:?}");

    let (inv, _wrote) = attached(&sweep, &crate::files::FileLedger::new());
    let edit = inv.edits.first().expect("a diff");
    assert_eq!(edit.path, "a.txt");
    assert_eq!(edit.before, "one\ntwo\n");
    assert_eq!(edit.after, "one\nTWO\n");
    let _ = std::fs::remove_dir_all(&root);
}

/// A `>` redirect onto a path git never had is a creation, not a rewrite.
#[test]
fn a_redirect_onto_a_new_path_is_a_creation() {
    let root = repo("redirect");
    let sweep = sweep_of(&root);
    std::fs::write(root.join("b.txt"), "new\n").expect("write");
    let (inv, _wrote) = attached(&sweep, &crate::files::FileLedger::new());
    let edit = inv.edits.first().expect("a diff");
    assert_eq!(edit.path, "b.txt");
    assert!(edit.created, "a file git never had was called a rewrite");
    assert_eq!(edit.before, "");
    assert_eq!(edit.after, "new\n");
    let _ = std::fs::remove_dir_all(&root);
}

/// A file that was already dirty is diffed against the bytes it held when the call started:
/// somebody else's earlier edit is not this command's.
#[test]
fn a_dirty_file_shows_only_this_commands_change() {
    let root = repo("dirty");
    std::fs::write(root.join("a.txt"), "one\ntwo edited by hand\n").expect("write");
    let sweep = sweep_of(&root);
    std::fs::write(root.join("a.txt"), "one\nthree edited by hand\n").expect("write");

    let (inv, _wrote) = attached(&sweep, &crate::files::FileLedger::new());
    let edit = inv.edits.first().expect("a diff");
    assert_eq!(edit.before, "one\ntwo edited by hand\n");
    assert_eq!(edit.after, "one\nthree edited by hand\n");
    let _ = std::fs::remove_dir_all(&root);
}

/// A command that changed nothing draws nothing, however dirty the tree already was.
#[test]
fn a_command_that_changed_nothing_attaches_nothing() {
    let root = repo("nothing");
    std::fs::write(root.join("a.txt"), "somebody else's work\n").expect("write");
    let sweep = sweep_of(&root);

    let (inv, wrote) = attached(&sweep, &crate::files::FileLedger::new());
    assert!(inv.edits.is_empty(), "{:?}", inv.notes);
    assert!(inv.notes.is_empty(), "{:?}", inv.notes);
    assert!(wrote.is_empty(), "{wrote:?}");
    let _ = std::fs::remove_dir_all(&root);
}

/// Fifteen files: the first twelve are drawn, and the three left over are named with the count -
/// a wide sweep draws diffs now, but never an unbounded pile of them.
#[test]
fn several_changed_files_are_each_drawn_up_to_the_cap_and_counted_past_it() {
    let root = repo("several");
    let sweep = sweep_of(&root);
    for i in 0..15 {
        std::fs::write(root.join(format!("f{i}.txt")), "new\n").expect("write");
    }

    let (inv, wrote) = attached(&sweep, &crate::files::FileLedger::new());
    assert_eq!(inv.edits.len(), MAX_DRAWN_DIFFS, "the cap holds");
    assert_eq!(
        wrote.len(),
        MAX_DRAWN_DIFFS,
        "only what is drawn is recorded"
    );
    let note = inv.notes.join("\n");
    assert!(note.contains("changed 15 files"), "{note}");
    assert!(note.contains("and 3 more files"), "{note}");
    assert!(note.contains("`git diff` is the one to read"), "{note}");
    let _ = std::fs::remove_dir_all(&root);
}

/// A rewrite that leaves the same bytes is not a change: an empty card reads as a bug.
#[test]
fn a_rewrite_that_leaves_the_same_bytes_draws_nothing() {
    let root = repo("same");
    let sweep = sweep_of(&root);
    std::fs::write(root.join("a.txt"), "one\ntwo\n").expect("write");

    let (inv, _wrote) = attached(&sweep, &crate::files::FileLedger::new());
    assert!(inv.edits.is_empty(), "{:?}", inv.notes);
    let _ = std::fs::remove_dir_all(&root);
}

/// Outside a git tree the sweep is off and the result is exactly what it was: no note, no card.
#[test]
fn a_workspace_in_no_git_tree_attaches_nothing() {
    let root =
        std::env::temp_dir().join(format!("letibot-sweep-not-a-repo-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("mkdir");
    std::fs::write(root.join("a.txt"), "one\n").expect("write");
    let sweep = Sweep::before(root.clone(), Some(root.clone()));
    std::fs::write(root.join("a.txt"), "two\n").expect("write");

    let (inv, wrote) = attached(&sweep, &crate::files::FileLedger::new());
    assert!(inv.edits.is_empty());
    assert!(inv.notes.is_empty(), "{:?}", inv.notes);
    assert!(wrote.is_empty());
    let _ = std::fs::remove_dir_all(&root);
}

/// The path the ledger is marked under is the one the session would use: relative to its
/// workspace when the file is under it, absolute when the command wrote outside it.
#[test]
fn the_spelling_is_the_sessions_own() {
    let root = repo("spelling");
    let sweep = sweep_of(&root);
    std::fs::write(root.join("fuzz/list.inc"), "changed\n").expect("write");
    let files = crate::files::FileLedger::new();
    let (inv, wrote) = attached(&sweep, &files);
    assert_eq!(
        inv.edits.first().map(|e| e.path.as_str()),
        Some("fuzz/list.inc")
    );
    assert_eq!(wrote[0].0, "fuzz/list.inc");

    // The same tree read from a session that sits elsewhere in it: the path is still the
    // one that file has under the workspace.
    let elsewhere = Sweep::before(root.clone(), Some(root.join("fuzz")));
    std::fs::write(root.join("fuzz/list.inc"), "changed again\n").expect("write");
    let (inv, wrote) = attached(&elsewhere, &crate::files::FileLedger::new());
    assert_eq!(
        inv.edits.first().map(|e| e.path.as_str()),
        Some("fuzz/list.inc")
    );
    assert_eq!(
        wrote[0].0, "list.inc",
        "relative to where that session sits"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// A command that changed a file is not adjudicated as an edit, so the row names the tools
/// that would have been: and only the ones this seat has.
#[test]
fn the_note_names_only_the_writing_tools_the_seat_has() {
    let root = repo("nudge");
    let sweep = sweep_of(&root);
    std::fs::write(root.join("a.txt"), "one\nchanged\n").expect("write");

    let mut inv = Invocation::ok("ran");
    let files = crate::files::FileLedger::new();
    attach_changes(
        &mut inv,
        &sweep,
        &files,
        crate::runtime::WritesWith {
            edit: false,
            write: false,
        },
        &|_, _| {},
    );
    let note = inv.notes.join("\n");
    assert!(note.contains("changed `a.txt`"), "{note}");
    assert!(
        !note.contains("judges a change before it lands"),
        "a seat with no writing tool was given advice it cannot take: {note}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// The capture trails the pipe, so the beat a block is first seen on can hold nothing of what
/// the program already wrote - and a card is never revised once it is up. The first reading
/// warms the decision and the second makes it; a run that wrote nothing still gets its card.
#[test]
fn a_card_is_raised_from_a_reading_that_stood_still_for_a_beat() {
    let line = (16u64, Some("Continue? [Y/n]".to_string()));
    let mut told = Told::default();
    assert!(!told.speaks_now(&line), "no card from the first beat alone");
    assert!(
        told.speaks_now(&line),
        "the same reading on the next beat does"
    );
    assert!(
        !told.speaks_now(&line),
        "and the reading a card was raised from is not raised twice"
    );

    let empty = (0u64, None);
    let mut silent = Told::default();
    assert!(!silent.speaks_now(&empty));
    assert!(
        silent.speaks_now(&empty),
        "a program that wrote nothing is a card too, once the beat has shown it is not a lag"
    );
}

/// A name is never invented from the command, which is the whole point of the parameter: a
/// guessed slug is worse than none, since the command is already on the row. A test where
/// the command is the shape a deriver would reach for must answer no name at all.
#[test]
fn no_slug_is_ever_derived_from_the_command() {
    let deriver_bait = serde_json::json!({
        "command": "W=/tmp/build; cd /tmp && cargo test --workspace --release",
        "background": true,
    });
    assert_eq!(slug_arg(&deriver_bait), None);

    // Every shape of *nobody said one*: the key absent, null, an empty string, and
    // whitespace. All four are the same answer, because all four are the same claim.
    for args in [
        serde_json::json!({"command": "cargo test"}),
        serde_json::json!({"command": "cargo test", "slug": null}),
        serde_json::json!({"command": "cargo test", "slug": ""}),
        serde_json::json!({"command": "cargo test", "slug": "   \t "}),
    ] {
        assert_eq!(slug_arg(&args), None, "{args}");
    }
}

/// A stated name is the caller's own words, spelled as a name: one stated name has to be one
/// row in `/job` and one cell in the pane, so runs of whitespace become `-` and the length is
/// capped. Nothing else moves: no case change, no fallback word.
#[test]
fn a_stated_name_is_flattened_into_a_name_and_kept_otherwise() {
    let args = |v: &str| serde_json::json!({"command": "cargo test", "slug": v});
    assert_eq!(
        slug_arg(&args("release-build")),
        Some("release-build".to_string())
    );
    assert_eq!(
        slug_arg(&args("  Release Build  ")),
        Some("Release-Build".to_string()),
        "whitespace is not a name's business, and the caller's case is"
    );
    assert_eq!(
        slug_arg(&args("fold\ntests")),
        Some("fold-tests".to_string()),
        "a name that would draw as two rows"
    );
    assert_eq!(
        slug_arg(&args("a\u{7}b")),
        Some("a-b".to_string()),
        "a control character is a break, not a letter: it cannot be drawn and it is not \
             part of a name"
    );
    let long = "x".repeat(200);
    let capped = slug_arg(&args(&long)).unwrap();
    assert_eq!(capped.chars().count(), crate::exec::MAX_JOB_NAME);
    assert!(capped.chars().all(|c| c == 'x'));
    // And the cap does not leave a dangling separator behind.
    let words = format!("{} tail", "word ".repeat(20));
    let capped = slug_arg(&args(&words)).unwrap();
    assert!(!capped.ends_with('-'), "{capped:?}");
    assert!(capped.chars().count() <= crate::exec::MAX_JOB_NAME);
}

/// The rule is [`crate::exec::job_name`]'s and this argument is a reader of it. The rule lives
/// outside this file so the daemon's own names for the work it starts could be the same rule
/// rather than a second spelling of one: a name has to read the same in the pane and `/job`.
#[test]
fn the_slug_argument_reads_the_shared_rule_and_does_not_re_spell_it() {
    for raw in [
        "release-build",
        "  Release Build  ",
        "fold\ntests",
        "a\u{7}b",
        "",
        "   \t ",
        &"x".repeat(200),
        &format!("{} tail", "word ".repeat(20)),
    ] {
        assert_eq!(
            slug_arg(&serde_json::json!({"command": "cargo test", "slug": raw})),
            crate::exec::job_name(raw),
            "{raw:?}"
        );
    }
}

#[test]
fn the_tail_is_what_survives_the_cap() {
    let text: String = (0..100).map(|i| format!("line {i}\n")).collect();
    let (kept, capped) = tail_lines(&text, 10, 100_000);
    assert!(capped);
    assert!(kept.starts_with("line 90"), "{kept}");
    assert!(kept.ends_with("line 99"), "{kept}");
}

#[test]
fn a_cut_lands_on_a_line_boundary() {
    let text = "aaaaaaaaaa\nbbbbbbbbbb\ncccccccccc\n";
    let (kept, capped) = tail_lines(text, 100, 15);
    assert!(capped);
    // No partial line: whatever survived is whole.
    assert!(text.lines().any(|l| l == kept.lines().next().unwrap()));
}

/// With auto-background on, the run the default would have killed is promoted instead; an
/// explicit `timeout_ms` still bounds the foreground part.
#[test]
fn auto_background_replaces_the_default_deadline_not_an_asked_one() {
    assert_eq!(deadline_for(None, false, false, true), None);
    assert_eq!(
        deadline_for(Some(5_000), false, false, true),
        Some(Duration::from_secs(5))
    );
    assert!(deadline_for(None, false, false, false).is_some());
}

#[test]
fn a_models_run_gets_the_default_and_what_it_asked_for() {
    assert_eq!(
        deadline_for(None, false, false, false),
        Some(Duration::from_millis(DEFAULT_TIMEOUT_MS))
    );
    assert_eq!(
        deadline_for(Some(30_000), false, false, false),
        Some(Duration::from_secs(30))
    );
}

#[test]
fn an_asked_deadline_is_clamped_to_one_ms_and_the_ceiling() {
    assert_eq!(
        deadline_for(Some(0), false, false, false),
        Some(Duration::from_millis(1))
    );
    assert_eq!(
        deadline_for(Some(u64::MAX), false, false, false),
        Some(Duration::from_millis(600_000))
    );
}

/// `wake` is `now`, `later` or absent, and anything else is refused: a misspelt `now` that
/// silently meant the default would be a result the model waits for and never gets.
#[test]
fn wake_is_now_or_later_and_the_result_says_which() {
    assert_eq!(wake_arg(&serde_json::json!({})).ok(), Some(None));
    assert_eq!(
        wake_arg(&serde_json::json!({"wake": "later"})).ok(),
        Some(Some(WantedNow::WakeLater)),
        "`later` is the model parking the job, not the default"
    );
    assert_eq!(
        wake_arg(&serde_json::json!({"wake": "now"})).ok(),
        Some(Some(WantedNow::WakeNow))
    );
    for bad in [serde_json::json!("soon"), serde_json::json!(true)] {
        assert!(
            wake_arg(&serde_json::json!({ "wake": bad })).is_err(),
            "{bad} was not refused"
        );
    }
}

#[test]
fn a_deadline_kill_names_one_way_out() {
    // The kill says background, and never "a larger timeout_ms", which the slow-run note on
    // the same tool calls a longer wait. It also says the output is partial: a run cut at its
    // deadline did not finish, and a model that reads it as finished builds on half of it.
    let text = deadline_killed("out", "j3", 120.0);
    assert!(
        text.contains("[`j3`] Killed at its 120s deadline — its output is partial"),
        "{text}"
    );
    assert!(
        text.ends_with(
            "Re-run it with `background: true`, which has no \
             deadline of its own."
        ),
        "{text}"
    );
    assert!(deadline_killed("out", "j3", 0.3).contains("its 300ms deadline"));
    assert!(!text.contains("timeout_ms"), "{text}");
}

#[test]
fn a_background_run_has_no_deadline() {
    assert_eq!(deadline_for(None, true, false, false), None);
}

#[test]
fn the_operators_own_run_has_no_deadline_whatever_the_call_says() {
    // The measured case: the daemon mints the operator's call with `command` and nothing
    // else, so `asked_ms` is `None`, and the run died at its silent default anyway.
    assert_eq!(deadline_for(None, false, true, false), None);
    // A `timeout_ms` no head can even pass today is still not a deadline: the person's run is
    // the person's.
    assert_eq!(deadline_for(Some(5_000), false, true, false), None);
}

/// The boundary is one minute, and both sides of it are pinned without waiting a real minute:
/// the decision is pure, so a nanosecond either side of [`SLOW_FOREGROUND`] stands in for it.
#[test]
fn a_foreground_run_owes_the_reminder_past_one_minute_and_not_before() {
    assert_eq!(
        slow_foreground(Duration::from_secs(59), false, SLOW_FOREGROUND),
        None
    );
    assert_eq!(
        slow_foreground(
            SLOW_FOREGROUND - Duration::from_millis(1),
            false,
            SLOW_FOREGROUND
        ),
        None,
        "the reminder fires MORE than a minute in, not at the minute"
    );
    let note = slow_foreground(SLOW_FOREGROUND, false, SLOW_FOREGROUND)
        .expect("a run that reaches the minute owes its reader the reminder");
    // The advice names the tool's own feature and the escape hatch it replaces: a model that
    // raised `timeout_ms` has to be told why that was the wrong knob, not merely that another
    // exists.
    assert!(note.contains("background: true"), "{note}");
    assert!(note.contains("timeout_ms"), "{note}");
    assert!(note.contains("completion is delivered"), "{note}");
}

/// The operator's own run is exempt: a person is watching it, and the seat that reads its
/// result is that person. The reminder is for a model deciding how to spell its next call.
#[test]
fn the_operators_own_run_is_not_reminded_even_past_the_minute() {
    assert_eq!(
        slow_foreground(Duration::from_secs(600), true, SLOW_FOREGROUND),
        None
    );
    // And the contrast: a model's run of the same age owes it.
    assert!(slow_foreground(Duration::from_secs(600), false, SLOW_FOREGROUND).is_some());
}

/// The description states the default in force and which file set it: the project's
/// `leticode.toml` when it spoke, the built-in otherwise.
#[test]
fn the_description_names_the_default_in_force_and_where_it_came_from() {
    use crate::exec::memory::{Amount, PolicySource};
    let built_in = Bash::default().schema();
    assert!(
        built_in
            .description
            .contains("runs under 64 MiB** (the built-in default)"),
        "{}",
        built_in.description
    );
    let project = Bash::with_memory(MemoryPolicy {
        default: Amount::Bytes(4 << 30),
        default_from: PolicySource::Project,
        ..MemoryPolicy::default()
    })
    .schema();
    assert!(
        project
            .description
            .contains("runs under 4 GiB** (`[memory] default` in this project's leticode.toml)"),
        "{}",
        project.description
    );
    let arg = project.parameters["properties"]["memory"]["description"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        arg.contains("runs under 4 GiB — `[memory] default` in this project's leticode.toml"),
        "{arg}"
    );
}

/// A job as the note sees it: a cap, and what the kernel counted for it once it settled.
fn settled(
    cap: crate::exec::MemoryCap,
    used: Option<crate::exec::scope::MemoryUse>,
) -> crate::exec::jobs::Job {
    use crate::exec::jobs::{Job, JobId, Lifetime, Stdin};
    use crate::exec::scope::{ScopeId, ScopeKind};
    let scope = |name: &str| ScopeId {
        kind: ScopeKind::Turn,
        name: name.into(),
        path: std::path::PathBuf::from("/nonexistent"),
    };
    let job = Job::new(
        JobId::next(),
        "cargo build".into(),
        None,
        Lifetime {
            scope: scope("turn"),
            owner: scope("session"),
            background: None,
        },
        "/w".into(),
        0,
        4096,
        Stdin::none(),
    )
    .with_memory_cap(cap);
    if let Some(used) = used {
        job.record_memory(used);
    }
    job
}

/// A call that ended nowhere near its cap says nothing about memory: a peak on every result is
/// a number to size work by, which the model is not given (R93.F1).
#[test]
fn a_finished_call_says_nothing_about_its_peak() {
    use crate::exec::MemoryCap;
    use crate::exec::scope::MemoryUse;
    let job = settled(
        MemoryCap::Capped(64 << 20),
        Some(MemoryUse {
            cap: Some(64 << 20),
            peak: 12 << 20,
            oom_kills: 0,
            ..MemoryUse::default()
        }),
    );
    assert_eq!(
        memory_note(&job),
        None,
        "an ordinary call reports no numbers at all"
    );
}

/// A box with no user systemd runs the call uncapped, and the result says so - the ask that
/// could not be honoured, the reason, and what the run peaked at - never a silent uncapped run.
#[test]
fn an_uncapped_call_names_the_ask_and_the_reason() {
    use crate::exec::MemoryCap;
    use crate::exec::scope::MemoryUse;
    let job = settled(
        MemoryCap::Uncapped {
            asked: 512 << 20,
            why: "no memory controller".into(),
        },
        Some(MemoryUse {
            cap: None,
            peak: 200 << 20,
            oom_kills: 0,
            ..MemoryUse::default()
        }),
    );
    let note = memory_note(&job).expect("a cap that could not be set is said");
    assert!(note.contains("ran UNCAPPED"), "{note}");
    assert!(note.contains("512 MiB was asked for"), "{note}");
    assert!(note.contains("no memory controller"), "{note}");
    assert!(note.contains("it peaked at 200 MiB"), "{note}");
}

#[test]
fn the_description_declares_exec_and_says_how_to_use_the_tool() {
    let s = Bash::default().schema();
    assert_eq!(s.access, Access::Exec);
    assert!(!s.access.is_unattended(), "exec must reach the gate");
    assert_eq!(crate::schema::lint_description(&s.description), vec![]);
}

const GIB: u64 = 1 << 30;

/// The machine R155 was reported on: 123 GiB, 15 GiB free when the ask was made.
fn on_lubuntu3(free_gib: u64) -> Machine {
    Machine::There {
        host: "lubuntu3".into(),
        mem: Some(crate::exec::memory::Meminfo {
            total: 123 * GIB,
            available: free_gib * GIB,
            swap_free: 0,
        }),
    }
}

fn on_a_silent_lubuntu3() -> Machine {
    Machine::There {
        host: "lubuntu3".into(),
        mem: None,
    }
}

/// `cargo clippy` asking for 12 GiB, the call in R155's report.
fn clippy_12g() -> Value {
    serde_json::json!({"command": "cargo clippy --workspace", "memory": "12G"})
}

/// What `memory_for` decides for one call, with the card recorded instead of answered.
fn decided(
    machine: &Machine,
    args: &Value,
    answer: MemoryGrant,
) -> (Result<Option<u64>, Box<Invocation>>, Vec<MemoryAsk>) {
    let mut cards: Vec<MemoryAsk> = Vec::new();
    let got = decide_memory(
        &MemoryPolicy::default(),
        "cargo clippy --workspace",
        args,
        machine,
        &mut |q| {
            cards.push(q.clone());
            answer.clone()
        },
    );
    (got, cards)
}

/// A placement's cap is decided from the host's numbers and the card says which host they are:
/// R155's whole report is a card quoting the daemon's box for a command that ran elsewhere.
#[test]
fn a_placements_ask_is_decided_from_its_host_and_the_card_names_it() {
    let (got, cards) = decided(&on_lubuntu3(15), &clippy_12g(), MemoryGrant::Granted);
    assert_eq!(
        got.expect("the operator let it have the memory"),
        Some(12 * GIB)
    );
    assert_eq!(cards.len(), 1, "an ask above the ceiling is one card");
    let card = &cards[0];
    // Half of lubuntu3's 15 GiB, which is not half of the daemon's box.
    assert_eq!(card.ceiling, Some(7 * GIB + GIB / 2));
    assert!(
        card.summary()
            .contains("15 GiB of 123 GiB is free on lubuntu3"),
        "{}",
        card.summary()
    );
    assert!(!card.summary().contains("185"), "{}", card.summary());
}

/// And a LOCAL call is decided from this box, with the card it has always worded: no machine
/// named, and the same figures. A change here is a change to every session on this box.
#[test]
fn a_local_ask_is_unchanged() {
    let local = Machine::Here(Some(crate::exec::memory::Meminfo {
        total: 185 * GIB,
        available: 15 * GIB,
        swap_free: 0,
    }));
    let (_, cards) = decided(&local, &clippy_12g(), MemoryGrant::Granted);
    assert_eq!(
        cards[0].summary(),
        "the model asks for 12 GiB of memory for `cargo clippy --workspace` — above the 7.5 \
             GiB it may take without asking; 15 GiB of 185 GiB is free now"
    );
}

/// A host that does not answer ASKS: an amount nobody can compare is the operator's, and both
/// the card and the refusal name the host instead of quoting this box's figures.
#[test]
fn a_silent_host_asks_rather_than_being_sized_from_this_box() {
    let (got, cards) = decided(&on_a_silent_lubuntu3(), &clippy_12g(), MemoryGrant::Granted);
    assert_eq!(got.expect("the operator granted it"), Some(12 * GIB));
    assert_eq!(cards.len(), 1, "an unknown machine is still a question");
    let said = cards[0].summary();
    assert!(said.contains("lubuntu3 did not answer"), "{said}");
    assert!(!said.contains("is free"), "{said}");
    assert_eq!(
        cards[0].ceiling, None,
        "nothing was read to compare against"
    );

    // Refused, the sentence names the host's silence as the reason there is no ceiling.
    let (got, _) = decided(
        &on_a_silent_lubuntu3(),
        &clippy_12g(),
        MemoryGrant::Refused("not on that machine".into()),
    );
    let refused = got.expect_err("a refusal does not run");
    assert!(
        refused.payload.contains("any ceiling at all")
            && refused.payload.contains("not on that machine"),
        "{}",
        refused.payload
    );
}

/// A fraction of what is free cannot be resolved on a host that did not answer, so the call is
/// refused with a sentence the model can act on: there is no number to ask a person for.
#[test]
fn a_fraction_of_an_unread_machines_memory_is_refused_with_the_hosts_name() {
    let args = serde_json::json!({"command": "cargo build", "memory": "50%"});
    let (got, cards) = decided(&on_a_silent_lubuntu3(), &args, MemoryGrant::Granted);
    assert!(
        cards.is_empty(),
        "there is nothing to put to a person: {cards:?}"
    );
    let refused = got.expect_err("a fraction of an unknown machine does not run");
    assert!(
        refused.payload.contains("lubuntu3") && refused.payload.contains("a size"),
        "{}",
        refused.payload
    );
}

/// A background job has no deadline of its own, and the argument list says so rather than
/// leaving a model to infer it from work that ended at a number nobody chose.
#[test]
fn the_background_arguments_say_a_job_has_no_deadline_of_its_own() {
    let s = Bash::default().schema();
    let arg = |name: &str| {
        s.parameters["properties"][name]["description"]
            .as_str()
            .unwrap_or_else(|| panic!("`{name}` has a description"))
            .to_string()
    };
    let background = arg("background");
    assert!(
        background.contains("no deadline of its own"),
        "{background}"
    );
    assert!(
        background.contains("job_dead_after_seconds"),
        "the window a silent job is measured against is named: {background}"
    );
    assert!(background.contains("job_extend"), "{background}");
    let timeout = arg("timeout_ms");
    assert!(timeout.contains("no deadline of its own"), "{timeout}");
    assert!(timeout.contains("job_extend"), "{timeout}");
}

/// A failed command whose output names a path in the letibot store is answered with the tools that
/// do hold it. The store's session directories are a file tool's, so an ENOENT there reads as a
/// typo unless something says otherwise - which is how a pane's own source came to look unreachable.
#[test]
fn a_failed_command_that_names_a_store_path_is_answered_with_the_file_tools() {
    let store = std::path::Path::new("/home/op/.local/share/letibot");
    let named = "cat: /home/op/.local/share/letibot/sessions/s-1/plugins/fuzzlegs/plugin.toml: \
                 No such file or directory";
    let noted = store_note(Invocation::failed("`cat` exited 1", named), true, store);
    let said = noted.notes.join(" ");
    assert!(
        said.contains("/sessions/s-1/plugins/fuzzlegs/plugin.toml"),
        "the path the command named: {said}"
    );
    for tool in ["`read`", "`edit`", "`write`"] {
        assert!(said.contains(tool), "{tool} is named as the way in: {said}");
    }
    assert!(
        said.contains("`bash`"),
        "and why a command could not: {said}"
    );

    // A command that did not fail is not answered with a note: it may have read the store as data.
    let ok = store_note(Invocation::ok(named), false, store);
    assert!(ok.notes.is_empty(), "{:?}", ok.notes);
    // And a failure that names a path outside the store earns nothing.
    let elsewhere = store_note(
        Invocation::failed(
            "`cat` exited 1",
            "cat: src/lib.rs: No such file or directory",
        ),
        true,
        store,
    );
    assert!(elsewhere.notes.is_empty(), "{:?}", elsewhere.notes);
}
