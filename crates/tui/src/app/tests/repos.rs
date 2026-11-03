//! A workspace of several repositories, none of them the main one: the header's git field follows
//! the repository the work is in, says how many there are before there is any work, and a click on
//! it opens the repo card.

use super::*;

struct Folder(std::path::PathBuf);

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `stroppy-io` with three repositories in it, each with one commit on `main`.
fn folder(tag: &str) -> Folder {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let root = letibot_sessionlog::testing::scratch_root()
        .join(format!("letibot-repos-{tag}-{}-{n}", std::process::id()))
        .join("stroppy-io");
    let _ = std::fs::remove_dir_all(&root);
    for r in ["stroppy", "stroppy-contrib", "website"] {
        let d = root.join(r);
        std::fs::create_dir_all(d.join("src")).unwrap();
        std::fs::write(d.join("src/x.rs"), "fn x() {}\n").unwrap();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["add", "-A"],
            &["commit", "-qm", "first"],
        ] {
            let ok = letibot_tools::testing::git(&d)
                .args(args)
                .output()
                .unwrap()
                .status
                .success();
            assert!(ok, "git {args:?}");
        }
    }
    Folder(root.parent().unwrap().to_path_buf())
}

fn at(f: &Folder) -> App {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.wiring.workspace = f.0.join("stroppy-io").display().to_string();
    a
}

/// The same, for a folder that is its own workspace (`one_repo`).
fn at_folder(f: &Folder) -> App {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.wiring.workspace = f.0.display().to_string();
    a
}

/// A row announced and then given its body, as the daemon delivers it.
fn row(a: &mut App, id: &str, kind: &str, item: TranscriptItem) {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    a.apply(ServerFrame::Event(env(seq, testing::appended(id, kind))));
    a.record_item(id, item);
}

fn edited(a: &mut App, id: &str, path: &str) {
    row(
        a,
        id,
        "tool_result",
        TranscriptItem::ToolResult {
            call_id: format!("c-{id}"),
            name: "edit".into(),
            outcome: letibot_transcript::ToolOutcome::Ok,
            payload: "done".into(),
            edit: Some(letibot_transcript::ToolEditExcerpt {
                path: path.into(),
                created: false,
                before_start: 1,
                after_start: 1,
                before_lines: 1,
                after_lines: 1,
                truncated: false,
                before: "fn x() {}".into(),
                after: "fn y() {}".into(),
            }),
            origin: None,
            media: None,
        },
    );
}

/// The header once the reading the loop started has been collected - `refresh_git` reads on a
/// thread and picks the answer up on a later tick.
fn header(a: &mut App) -> String {
    a.git_read.1 = 0;
    a.refresh_git();
    for _ in 0..500 {
        if a.git_job.is_none() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
        a.refresh_git();
    }
    a.screen(140, 30)[0].clone()
}

#[test]
fn the_repositories_are_listed_with_the_same_named_one_first() {
    let f = folder("list");
    let ws = f.0.join("stroppy-io");
    std::fs::create_dir_all(ws.join("stroppy-io/src")).unwrap();
    letibot_tools::testing::git(&ws.join("stroppy-io"))
        .args(["init", "-q"])
        .output()
        .unwrap();
    let names: Vec<String> = crate::gitfield::repos_in(&ws.display().to_string())
        .iter()
        .map(|p| crate::app::pick::repo_name(p))
        .collect();
    assert_eq!(
        names,
        ["stroppy-io", "stroppy", "stroppy-contrib", "website"]
    );
}

#[test]
fn with_nothing_to_follow_the_field_says_how_many() {
    let f = folder("count");
    let mut a = at(&f);
    let h = header(&mut a);
    assert!(h.contains("(3 repos)"), "{h}");
}

#[test]
fn the_field_follows_the_newest_edit_then_a_command_into_another() {
    let f = folder("follow");
    let mut a = at(&f);
    edited(&mut a, "s.1", "stroppy/src/x.rs");
    let h = header(&mut a);
    assert!(
        h.contains("stroppy-io/stroppy"),
        "the path names the repository: {h}"
    );
    assert!(h.contains("(main"), "and the field is its branch: {h}");
    assert!(!h.contains("repos)"), "{h}");

    // A shell command that starts in another repository moves it there.
    row(
        &mut a,
        "s.2",
        "assistant",
        TranscriptItem::Assistant {
            text: String::new(),
            tool_calls: vec![letibot_transcript::ToolCall {
                id: "c2".into(),
                name: "bash".into(),
                arguments: r#"{"command": "cd website && make build"}"#.into(),
            }],
            truncated: false,
        },
    );
    let h = header(&mut a);
    assert!(h.contains("stroppy-io/website"), "{h}");
}

#[test]
fn a_click_on_the_field_opens_the_card_and_a_pin_holds_against_the_work() {
    let f = folder("pin");
    let mut a = at(&f);
    edited(&mut a, "s.1", "stroppy/src/x.rs");
    let frame = {
        header(&mut a);
        a.screen(140, 30)
    };
    let x = crate::app::keyhints::cells(&frame[0])
        .iter()
        .collect::<String>()
        .find("(main")
        .expect("the field is drawn");
    a.key(Key::Click {
        x: x as u16 + 1,
        y: 0,
    });
    assert_eq!(
        a.pick,
        Some(crate::app::pick::Pick::Repo),
        "the click opened the repo card"
    );
    let rows: Vec<String> = a.pick_values().into_iter().map(|(n, _)| n).collect();
    assert_eq!(
        rows,
        [
            crate::app::pick::FOLLOW,
            "stroppy",
            "stroppy-contrib",
            "website"
        ]
    );

    a.take_pick("website".into());
    edited(&mut a, "s.2", "stroppy-contrib/src/x.rs");
    let h = header(&mut a);
    assert!(
        h.contains("stroppy-io/website"),
        "the pin holds against newer work: {h}"
    );

    a.command("repo");
    a.take_pick(crate::app::pick::FOLLOW.into());
    let h = header(&mut a);
    assert!(
        h.contains("stroppy-io/stroppy-contrib"),
        "following again: {h}"
    );
}

/// A folder holding one repository and the operator's `TODO.md`: their own layout, the file beside
/// the repository one level down and the same name. `todo` decides whether the list sits in the
/// folder or beside the code, which is the split this test is about.
fn one_repo(tag: &str, todo: Option<&str>) -> Folder {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let root = letibot_sessionlog::testing::scratch_root()
        .join(format!("letibot-repos-{tag}-{}-{n}", std::process::id()))
        .join("letibot");
    let _ = std::fs::remove_dir_all(&root);
    let repo = root.join("letibot");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/x.rs"), "fn x() {}\n").unwrap();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["add", "-A"],
        &["commit", "-qm", "first"],
    ] {
        let ok = letibot_tools::testing::git(&repo)
            .args(args)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?}");
    }
    if let Some(text) = todo {
        std::fs::write(root.join("TODO.md"), text).unwrap();
    }
    Folder(root)
}

/// The corner's repository survives a `TODO.md` beside it: the branch is drawn beside the path, is
/// a click target, and the click opens the repo card. One directory answering both questions is
/// what broke the corner, so file and branch are two resolutions and the todos pane keeps the folder.
#[test]
fn the_corners_repository_survives_a_todo_md_beside_it() {
    let f = one_repo(
        "corner",
        Some("# TODO\n\n## the corner\n\n- [ ] the corner's repo link\n"),
    );
    let mut a = at_folder(&f);
    let frame = {
        header(&mut a);
        a.screen(140, 30)
    };
    let drawn: String = crate::app::keyhints::cells(&frame[0]).iter().collect();
    assert!(
        drawn.contains("(main"),
        "the corner drew no repository: {drawn}"
    );
    // The path is the FOLDER (what the operator sees) and the field beside it is the repository
    // one level down - the two resolutions of one layout, each answering its own question.
    assert!(
        drawn.contains(&f.0.display().to_string()),
        "the path is not the session's workspace: {drawn}"
    );

    // **The field is a target, and its FIRST column is inside it.** The record was made two
    // columns left of the drawing (no `+ gutter`), which a click in the middle of a six-column
    // field survived and a click on its edge did not - so this clicks the edge.
    let hit = a
        .hits
        .iter()
        .find(|h| h.target == Target::GitField)
        .cloned()
        .expect("the corner's repository is not a click target");
    let col = drawn.find("(main").expect("the field is drawn");
    assert_eq!(
        hit.rect.x, col,
        "the target is not where the field was drawn: {drawn}"
    );
    a.key(Key::Click {
        x: hit.rect.x as u16,
        y: hit.rect.y as u16,
    });
    assert_eq!(
        a.pick,
        Some(crate::app::pick::Pick::Repo),
        "the click opened the repo card"
    );
    let rows: Vec<String> = a.pick_values().into_iter().map(|(n, _)| n).collect();
    assert_eq!(
        rows.first().map(String::as_str),
        Some(crate::app::pick::FOLLOW),
        "the card's first row is the option to track the work: {rows:?}"
    );

    // **And the list is still the folder's.** The split's other half: the pane reads the same
    // `TODO.md` the operator wrote, not the repository's (which does not exist here).
    let rows = crate::ui::panes::todos::repo_todos_map(&a.wiring.workspace);
    let text = rows
        .iter()
        .map(|r| r.text.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        text.contains("the corner's repo link"),
        "the pane read the wrong TODO.md: {text}"
    );

    // **And a list beside the code is still read from there.** The fallback is what every layout
    // before 2026-10-31 used, so the split must not cost it: a folder with no `TODO.md` of its
    // own opens the repository's.
    let g = one_repo("beside", None);
    let beside = g.0.join("letibot");
    std::fs::write(
        beside.join("TODO.md"),
        "# TODO\n\n## beside the code\n\n- [ ] the repository's own list\n",
    )
    .unwrap();
    let rows = crate::ui::panes::todos::repo_todos_map(&g.0.display().to_string());
    let text = rows
        .iter()
        .map(|r| r.text.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        text.contains("the repository's own list"),
        "the fallback to the project's TODO.md is gone: {text}"
    );
}

#[test]
fn a_call_names_its_path_its_directory_or_where_its_command_goes() {
    use crate::gitfield::call_path;
    assert_eq!(call_path(r#"{"path":"a/b.rs"}"#).as_deref(), Some("a/b.rs"));
    assert_eq!(
        call_path(r#"{"command":"ls","cwd":"web"}"#).as_deref(),
        Some("web")
    );
    assert_eq!(
        call_path(r#"{"command":"cd 'site' && npm test"}"#).as_deref(),
        Some("site")
    );
    assert_eq!(call_path(r#"{"command":"cargo test"}"#), None);
    assert_eq!(call_path(r#"{"path":"."}"#), None);
}
