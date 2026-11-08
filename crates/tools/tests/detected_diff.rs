//! A `bash` call that writes a file is drawn as the edit it is: the sweep reads the git tree the
//! command ran in, before and after, and attaches both sides of every file the command changed.
//! Every test drives the real tool and a real shell, because the loss this guards was in the
//! seam between them and a fixture on either side of it would hide it.

use letibot_tools::testing::{Harness, git, runner_harness, writing_runner_harness};

/// The tree every test reasons about: a repository with committed files, and a shell that can
/// rewrite them. A box with no process-lifetime tree skips loudly rather than passing on nothing.
fn repo(name: &str) -> Option<(Harness, std::path::PathBuf)> {
    let h = match writing_runner_harness() {
        Ok(h) => h,
        Err(e) => {
            eprintln!("{name}: no cgroup v2 subtree here, skipping: {e}");
            let msg = format!("{e}");
            assert!(
                msg.contains("nothing would reap it") || msg.contains("cgroup"),
                "a refusal must name what was missing: {msg}"
            );
            let _ = letibot_tokencore::apparatus::present(
                "a cgroup v2 subtree this user may write",
                false,
            );
            return None;
        }
    };
    let root = h.root().to_path_buf();
    std::fs::create_dir_all(root.join("fuzz")).expect("mkdir");
    h.write_file("a.txt", "one\ntwo\n");
    h.write_file("fuzz/list.inc", "alpha\nbeta\n");
    git_ok(&root, &["init", "-q"]);
    git_ok(&root, &["add", "-A"]);
    git_ok(&root, &["commit", "-qm", name]);
    Some((h, root))
}

fn git_ok(root: &std::path::Path, args: &[&str]) {
    let out = git(root).args(args).output().expect("git");
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

/// The operator's shape: an inline python program that reads a tracked file and writes it back.
#[test]
fn a_python_inline_program_that_rewrites_a_tracked_file_attaches_its_diff() {
    let Some((mut h, root)) = repo("python") else {
        return;
    };
    let command = "python3 -c \"p = 'a.txt'; s = open(p).read(); \
                   open(p, 'w').write(s + 'three' + chr(10))\"";
    let r = h.call("bash", &format!("{{\"command\": {}}}", json(command)));

    let edit = r
        .edits
        .first()
        .unwrap_or_else(|| panic!("no diff attached: {}", r.render()));
    assert_eq!(edit.path, "a.txt");
    assert_eq!(edit.before, "one\ntwo\n");
    assert_eq!(edit.after, "one\ntwo\nthree\n");
    assert!(!edit.created);
    assert!(
        r.render().contains("changed `a.txt`"),
        "the model is told what moved: {}",
        r.render()
    );
    let _ = root;
}

/// `sed -i` is the other shape a model reaches for, and it rewrites the file under the sweep's
/// nose the same way.
#[test]
fn a_sed_in_place_rewrite_attaches_its_diff() {
    let Some((mut h, _root)) = repo("sed") else {
        return;
    };
    let r = h.call("bash", r#"{"command":"sed -i s/two/TWO/ a.txt"}"#);
    let edit = r
        .edits
        .first()
        .unwrap_or_else(|| panic!("no diff attached: {}", r.render()));
    assert_eq!(edit.path, "a.txt");
    assert_eq!(edit.before, "one\ntwo\n");
    assert_eq!(edit.after, "one\nTWO\n");
}

/// A `>` redirect onto a path git never had is a creation, not a rewrite.
#[test]
fn a_redirect_attaches_the_created_files_diff() {
    let Some((mut h, _root)) = repo("redirect") else {
        return;
    };
    let r = h.call("bash", r#"{"command":"echo new > b.txt"}"#);
    let edit = r
        .edits
        .first()
        .unwrap_or_else(|| panic!("no diff attached: {}", r.render()));
    assert_eq!(edit.path, "b.txt");
    assert!(edit.created, "a `>` onto a new path is a creation");
    assert_eq!(edit.before, "");
    assert_eq!(edit.after, "new\n");
}

/// A file that was already dirty is diffed against the bytes it held when the call started, not
/// against `HEAD`: somebody else's earlier edit is not this command's.
#[test]
fn a_file_that_was_already_dirty_shows_only_this_calls_change() {
    let Some((mut h, root)) = repo("dirty") else {
        return;
    };
    h.write_file("a.txt", "one\ntwo edited by hand\n");
    let r = h.call("bash", r#"{"command":"sed -i s/two/three/ a.txt"}"#);
    let edit = r
        .edits
        .first()
        .unwrap_or_else(|| panic!("no diff attached: {}", r.render()));
    assert_eq!(
        edit.before, "one\ntwo edited by hand\n",
        "the before side is the pre-call content, not HEAD"
    );
    assert_eq!(edit.after, "one\nthree edited by hand\n");
    let _ = root;
}

/// A command that writes nothing attaches nothing, however dirty the tree already was.
#[test]
fn a_command_that_writes_nothing_attaches_nothing() {
    let Some((mut h, _root)) = repo("nothing") else {
        return;
    };
    h.write_file("a.txt", "somebody else's work\n");
    let r = h.call("bash", r#"{"command":"git status --short"}"#);
    assert!(
        r.edits.is_empty(),
        "a read-only command drew a diff: {}",
        r.render()
    );
    assert!(
        !r.render().contains("this command changed"),
        "{}",
        r.render()
    );
}

/// A workspace that is not a git tree is left exactly as it was: no sweep, no note, no card.
#[test]
fn a_workspace_that_is_not_a_git_tree_is_unchanged() {
    let mut h = match runner_harness() {
        Ok(h) => h,
        Err(e) => {
            eprintln!("no cgroup v2 subtree here, skipping: {e}");
            let _ = letibot_tokencore::apparatus::present(
                "a cgroup v2 subtree this user may write",
                false,
            );
            return;
        }
    };
    h.write_file("a.txt", "one\ntwo\n");
    let r = h.call("bash", r#"{"command":"echo three >> a.txt"}"#);
    assert!(r.edits.is_empty(), "{}", r.render());
    assert!(
        !r.render().contains("this command changed"),
        "{}",
        r.render()
    );
    // The command still ran: nothing about the sweep changes what a shell does.
    assert_eq!(h.read_file("a.txt"), "one\ntwo\nthree\n");
}

/// The operator's arango shape: the session sits somewhere that is not the repository, and the
/// command `cd`s into it, so the tree the shell wrote into is not the session's workspace.
#[test]
fn a_cd_into_another_checkout_is_the_tree_that_is_swept() {
    let Some((mut h, root)) = repo("elsewhere") else {
        return;
    };
    // A second checkout beside the workspace, with its own commit history.
    let other = root.join("other");
    std::fs::create_dir_all(&other).expect("mkdir");
    std::fs::write(other.join("b.txt"), "alpha\nbeta\n").expect("write");
    git_ok(&other, &["init", "-q"]);
    git_ok(&other, &["add", "-A"]);
    git_ok(&other, &["commit", "-qm", "other"]);

    let r = h.call(
        "bash",
        r#"{"command":"cd other && sed -i s/beta/BETA/ b.txt"}"#,
    );
    let edit = r
        .edits
        .first()
        .unwrap_or_else(|| panic!("no diff attached: {}", r.render()));
    assert_eq!(edit.path, "b.txt", "the path is relative to its own tree");
    assert_eq!(edit.before, "alpha\nbeta\n");
    assert_eq!(edit.after, "alpha\nBETA\n");
}

/// A rewrite that leaves the same bytes is not a change: an empty diff draws an empty card, which
/// reads as a bug.
#[test]
fn a_rewrite_that_leaves_the_same_bytes_attaches_nothing() {
    let Some((mut h, _root)) = repo("same") else {
        return;
    };
    let r = h.call(
        "bash",
        r#"{"command":"python3 -c \"p = 'a.txt'; s = open(p).read(); open(p, 'w').write(s)\""}"#,
    );
    assert!(r.edits.is_empty(), "{}", r.render());
}

/// The ledger learns the bytes this session's own command wrote, so a later `write` on the same
/// file is not told somebody else has been there.
#[test]
fn the_file_ledger_learns_what_a_command_wrote() {
    let Some((mut h, _root)) = repo("ledger") else {
        return;
    };
    let r = h.call("bash", r#"{"command":"sed -i s/two/THREE/ a.txt"}"#);
    assert!(!r.edits.is_empty(), "{}", r.render());
    let w = h.call(
        "write",
        r#"{"path":"a.txt","content":"one\nTHREE\nplus\n"}"#,
    );
    assert!(
        w.render().starts_with("`a.txt` rewritten") || w.render().contains("rewritten"),
        "the write refused a file this session's own command wrote: {}",
        w.render()
    );
}

/// How the tool made a v2 refspec safe for a JSON string: the command is handed to `bash` as an
/// argument, so a quote in it has to survive one pass of JSON.
fn json(s: &str) -> String {
    serde_json::to_string(s).expect("a command is a string")
}
