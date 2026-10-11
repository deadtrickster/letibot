//! **A todo row's `waits on …` is a link** — the operator: *"\"waits on j97\" - make j97 a link
//! to the real job, also render job slug"*. The slug (and a child's short name) on the row and in
//! its detail view, the click on exactly those words opening the job's output or reading the
//! child, `w` doing the same from the keyboard, and the cases that must open nothing wrong.

use super::*;
use letibot_sessionlog::event::{TodoBy, TodoCondition, TodoEntry, TodoNeed, TodoStatus};

const CHILD: &str = "s-1760000000-sub-1234abcd";

fn job(id: &str, slug: &str) -> letibot_sessionlog::protocol::JobEntry {
    letibot_sessionlog::protocol::JobEntry {
        id: id.into(),
        command: "cargo test".into(),
        slug: slug.into(),
        how: "asked".into(),
        state: "running".into(),
        running: true,
        never_ran: false,
        redirect: None,
        produced: 0,
        elapsed_ms: 10,
    }
}

fn row(content: &str, when: Option<&str>, needs: Vec<TodoNeed>) -> TodoEntry {
    TodoEntry {
        content: content.into(),
        status: TodoStatus::Pending,
        by: TodoBy::Model,
        when: when.map(|h| TodoCondition::Job { handle: h.into() }),
        needs,
    }
}

/// A board with one row of each kind of wait, the jobs the head knows, and the child it knows,
/// with the todos pane open.
fn board() -> App {
    let mut a = app();
    a.session_id = "s1".into();
    a.apply(ServerFrame::Jobs {
        session_id: "s1".into(),
        jobs: vec![job("j97", "pg-timing"), job("j96", "bench"), job("j95", "")],
    });
    a.apply(ServerFrame::Todos {
        session_id: "s1".into(),
        todos: vec![
            row(
                "build the index",
                None,
                vec![TodoNeed::Job {
                    handle: "j97".into(),
                }],
            ),
            row("time the queries", Some("j96"), Vec::new()),
            row(
                "review the port",
                None,
                vec![TodoNeed::Child { id: CHILD.into() }],
            ),
            row(
                "chase a ghost",
                None,
                vec![TodoNeed::Job {
                    handle: "j404".into(),
                }],
            ),
            row(
                "unnamed job",
                None,
                vec![TodoNeed::Job {
                    handle: "j95".into(),
                }],
            ),
            row("plain row", None, Vec::new()),
        ],
    });
    a.key(Key::CtrlT);
    a.subagents = vec![SubagentState {
        session_id: CHILD.into(),
        state: "running".into(),
        generating: true,
        prompt: "review it".into(),
        role: "reviewer".into(),
        task: String::new(),
        model: String::new(),
        answer: None,
        spawned_ms: 0,
    }];
    a
}

fn line_with<'a>(lines: &'a [String], words: &str) -> &'a str {
    lines
        .iter()
        .find(|l| l.contains(words))
        .unwrap_or_else(|| panic!("{words:?} is drawn: {lines:#?}"))
}

/// The screen row and column `needle` starts at, on the row that also holds `on_row`. Every
/// glyph the pane draws is one column wide, so a column is a count of characters.
fn at(frame: &[String], on_row: &str, needle: &str) -> (u16, u16) {
    let y = frame
        .iter()
        .position(|l| l.contains(on_row))
        .unwrap_or_else(|| panic!("{on_row:?} is drawn: {frame:#?}"));
    let l = &frame[y];
    let byte = l
        .find(needle)
        .unwrap_or_else(|| panic!("{needle:?} on {l:?}"));
    (l[..byte].chars().count() as u16, y as u16)
}

/// **The slug is drawn**: for a `needs` job edge and for a `when` job condition alike; a job
/// with no slug, and one the head has never heard of, draw the bare handle and nothing else; a
/// child draws the short name the subagents pane gives it, not its whole id.
#[test]
fn a_wait_is_drawn_with_the_jobs_slug_and_the_childs_short_name() {
    let mut a = board();
    let lines = a.todos_lines(160);
    assert!(
        line_with(&lines, "build the index").ends_with("waits on j97 · pg-timing"),
        "{lines:#?}"
    );
    assert!(
        line_with(&lines, "time the queries").ends_with("waits on j96 · bench"),
        "a when-job condition carries its slug too: {lines:#?}"
    );
    assert!(
        line_with(&lines, "review the port").ends_with("waits on …1234abcd · reviewer"),
        "{lines:#?}"
    );
    assert!(!line_with(&lines, "review the port").contains(CHILD));
    assert!(line_with(&lines, "chase a ghost").ends_with("waits on j404"));
    assert!(line_with(&lines, "unnamed job").ends_with("waits on j95"));
    assert!(!line_with(&lines, "plain row").contains("waits on"));
}

/// **The detail view says the same, and keeps the child's whole id** — the one place the id the
/// row shortened can still be read.
#[test]
fn the_detail_view_names_the_slug_and_keeps_the_childs_id() {
    let mut a = board();
    a.todos_lines(160);
    let read = |a: &mut App, words: &str| {
        a.todos_open = Some(TodoStop::Theirs(words.into()));
        a.todos_open_lines(160).join("\n")
    };
    let v = read(&mut a, "build the index");
    assert!(v.contains("needs job j97 · pg-timing"), "{v}");
    let v = read(&mut a, "time the queries");
    assert!(v.contains("waits on j96 · bench"), "{v}");
    let v = read(&mut a, "review the port");
    assert!(
        v.contains(&format!("needs child …1234abcd · reviewer ({CHILD})")),
        "{v}"
    );
}

/// **A click on the link opens the real job's output** — the view Enter on that job in the jobs
/// pane opens — and on a child's link reads the child, as the subagents pane's `p` does. The
/// cursor goes to the clicked row. Esc comes back to the todos.
#[test]
fn a_click_on_the_link_opens_the_job_and_the_child() {
    let mut a = board();
    let frame = a.screen(160, 40);
    let (x, y) = at(&frame, "build the index", "waits on");
    assert_eq!(
        a.key(Key::Click { x, y }),
        Some(Action::ReadJobOutput {
            job: "j97".into(),
            offset: 0
        })
    );
    assert_eq!(a.top_surface(), Some(Surface::JobOut));
    assert_eq!(a.job_out.as_ref().map(|j| j.job.as_str()), Some("j97"));
    assert_eq!(a.todos_sel, 1, "the cursor is on the clicked row");
    a.key(Key::Esc);
    assert_eq!(a.top_surface(), Some(Surface::Todos));

    // The last column of the slug is still the link.
    let frame = a.screen(160, 40);
    let (x, y) = at(&frame, "time the queries", "bench");
    assert_eq!(
        a.key(Key::Click { x: x + 4, y }),
        Some(Action::ReadJobOutput {
            job: "j96".into(),
            offset: 0
        }),
        "the when-job row's link opens its job"
    );
    a.key(Key::Esc);

    let frame = a.screen(160, 40);
    let (x, y) = at(&frame, "review the port", "…1234abcd");
    assert_eq!(
        a.key(Key::Click { x, y }),
        Some(Action::Peek(CHILD.into())),
        "the child's link reads that child, by its whole id"
    );
    assert_eq!(a.sub_out_pending.as_deref(), Some(CHILD));
}

/// **A click elsewhere on the row keeps today's behaviour**: it selects the row and opens
/// nothing — and so does a click on the ` · ` before `waits on`, and one just past the link.
#[test]
fn a_click_beside_the_link_only_selects_the_row() {
    let mut a = board();
    let frame = a.screen(160, 40);
    let (x, y) = at(&frame, "build the index", "build the index");
    assert_eq!(a.key(Key::Click { x, y }), None);
    assert_eq!(a.top_surface(), Some(Surface::Todos));
    assert_eq!(a.todos_sel, 1);

    let (x, y) = at(&frame, "build the index", " · waits on");
    assert_eq!(a.key(Key::Click { x, y }), None, "the dot is not the link");
    let (x, y) = at(&frame, "build the index", "pg-timing");
    assert_eq!(
        a.key(Key::Click {
            x: x + "pg-timing".len() as u16,
            y
        }),
        None,
        "past the link's end"
    );
    assert_eq!(a.top_surface(), Some(Surface::Todos));
    assert!(a.job_out.is_none());

    // The plain row has no link at all.
    let (x, y) = at(&frame, "plain row", "— model");
    assert_eq!(a.key(Key::Click { x, y }), None);
    assert_eq!(a.todos_sel, 6);
}

/// **`w` on the selected row does what the click does**, and the bar names it only on a row that
/// waits on something; on a row that waits on nothing it declines and the letter is the
/// composer's.
#[test]
fn w_on_the_selected_row_opens_what_it_waits_on() {
    let mut a = board();
    a.todos_lines(160);
    a.key(Key::Down);
    assert!(
        a.surface_hint(Surface::Todos)
            .contains("w opens the job it waits on"),
        "{}",
        a.surface_hint(Surface::Todos)
    );
    assert_eq!(
        a.key(Key::Char('w')),
        Some(Action::ReadJobOutput {
            job: "j97".into(),
            offset: 0
        })
    );
    a.key(Key::Esc);
    a.key(Key::Down);
    a.key(Key::Down);
    assert!(
        a.surface_hint(Surface::Todos)
            .contains("w reads the child it waits on")
    );
    assert_eq!(a.key(Key::Char('w')), Some(Action::Peek(CHILD.into())));

    let mut a = board();
    a.todos_sel = 6;
    assert!(!a.surface_hint(Surface::Todos).contains("w opens"));
    assert_eq!(a.key(Key::Char('w')), None);
    assert_eq!(a.top_surface(), Some(Surface::Todos));
    assert_eq!(a.editor.text(), "w", "declined, so the letter was typed");
}

/// **A job the head does not know opens nothing and says so** — no crash, and no other job in
/// its place; a job only a completion notice still names opens that notice whole.
#[test]
fn an_unknown_job_is_safe() {
    let mut a = board();
    let frame = a.screen(160, 40);
    let (x, y) = at(&frame, "chase a ghost", "waits on");
    assert_eq!(a.key(Key::Click { x, y }), None);
    assert_eq!(a.top_surface(), Some(Surface::Todos));
    assert!(a.job_out.is_none());
    assert!(
        a.notice
            .as_deref()
            .is_some_and(|n| n.contains("j404 is not in this session's job list")),
        "{:?}",
        a.notice
    );
    a.todos_sel = 4;
    assert_eq!(a.key(Key::Char('w')), None);
    assert!(a.job_out.is_none());

    // Not attached: nothing is asked of a daemon there is no session on.
    let mut a = board();
    a.screen(160, 40);
    a.session_id.clear();
    a.todos_sel = 1;
    assert_eq!(a.key(Key::Char('w')), None);
    assert!(a.job_out.is_none());
}

/// **A row cut at the pane's edge links only what is still on the screen**, and a row cut before
/// its `waits on` begins has no link at all.
#[test]
fn a_cut_row_links_only_its_visible_part() {
    let mut a = board();
    let lines = a.todos_lines(160);
    let whole = line_with(&lines, "build the index").chars().count();
    let link = a.todos_wait_links[0].clone();
    assert_eq!(link.to, whole);
    assert_eq!(
        link.to - link.from,
        "waits on j97 · pg-timing".chars().count()
    );

    // Cut four columns into the slug: the link ends where the ellipsis begins.
    let w = whole - 6;
    a.todos_lines(w);
    let cut = a.todos_wait_links[0].clone();
    assert_eq!((cut.from, cut.to), (link.from, w - 1));

    // Cut before `waits on`: no link for that row.
    a.todos_lines(link.from - 2);
    assert!(a.todos_wait_links.iter().all(|l| l.todo != 0));
}
