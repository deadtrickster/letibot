//! **The todos pane**: the session's plan and the repository's queue (`TODO.md`), and the
//! reader that turns `TODO.md` into its rows. The pane is drawn by `rano::agent::todos`; the
//! reader, the counts and which row the cursor is on are this head's.

use crate::app::*;
use crate::ui::render::row_strings;
use rano::agent::todos::{RepoRow, TodoLine, TodosPane};

impl App {
    pub(crate) fn todos_lines(&mut self, w: usize) -> Vec<String> {
        use letibot_sessionlog::event::{TodoBy, TodoCondition, TodoStatus};
        let (open, postponed) = todo_counts(&self.todos);
        // **Through the one enumeration**: the row the cursor is on is the stop at
        // `todos_sel` of the same list the keys act on, matched here by identity (the
        // operator's rows by their words, the file's by their place).
        let stops = self.todos_stops();
        let cursor = self.todos_sel.min(stops.len().saturating_sub(1));
        let marked = |want: &TodoStop| stops.get(cursor).is_some_and(|it| it == want);
        let mut mine_at = 0usize;
        let pane = TodosPane {
            open,
            postponed,
            on_add: marked(&TodoStop::Add),
            todos: self
                .todos
                .iter()
                .map(|t| {
                    let mine = t.by == TodoBy::Operator;
                    if mine {
                        mine_at += 1;
                    }
                    TodoLine {
                        mark: match t.status {
                            TodoStatus::Pending => TodoMark::Open,
                            TodoStatus::InProgress => TodoMark::Doing,
                            TodoStatus::Completed => TodoMark::Done,
                            TodoStatus::Postponed => TodoMark::Postponed,
                        },
                        content: t.content.clone(),
                        mine,
                        number: mine_at,
                        waits_on: match &t.when {
                            Some(TodoCondition::Job { handle }) => Some(handle.clone()),
                            None => None,
                        },
                        cursor: mine && marked(&TodoStop::Mine(t.content.clone())),
                    }
                })
                .collect(),
            repo: self.repo_todos.as_ref().map(|rows| {
                rows.iter()
                    .enumerate()
                    .map(|(i, r)| {
                        let here = r.item && marked(&TodoStop::Repo(i));
                        RepoRow {
                            indent: r.indent,
                            mark: r.mark,
                            text: r.text.clone(),
                            body: r.body.clone(),
                            item: r.item,
                            cursor: here,
                            open: here && self.repo_open,
                        }
                    })
                    .collect()
            }),
        };
        let content = pane.content(w);
        // **The rows the stops landed on, taken as they went out** — what the arrows scroll by
        // and a click is tested against.
        self.todos_stop_rows = content.stop_rows;
        row_strings(&content.lines, self.cfg.palette())
    }
}

/// The repo's `TODO.md` as a section map: one line per `##` section with its
/// open and done checkbox counts. Read fresh on every pane-open — the file is
/// the operator's to edit, and a cached map is a cache of somebody else's
/// intention. Errors name themselves; a missing file is a fact about the
/// workspace, not a panic in a pane.
/// **The repo's `TODO.md`, as org-mode reads a list of checkboxes.**
///
/// It used to render one line per `## ` heading with the counts beside it —
/// *"Phase 0 — repo — 0 open, 2 done"* — and nothing else. The items themselves,
/// which are the queue, were never drawn. The operator, looking at `leticl`'s:
/// *"our todo pane doesnt render them - only section titles and sub todos
/// count"*.
///
/// So the items are drawn under their heading, and the heading carries the state
/// org would give it: a parent is DONE when every child is, and org's own cookie
/// — `[2/7]` — says how far along the rest are. Three marks, matching the ones
/// this file's own legend defines and the ones `todo_write` uses, so the two
/// halves of this pane read alike:
///
/// ```text
///   [x] Phase 0 — repo                        [2/2]
///       [x] T1 git init, .gitignore, commit PLAN.md + TODO.md.
///       [x] T2 vendor yason + alexandria + trivial-gray-streams, pinned in
///   [~] Phase 7 — parity                      [3/9]
///       [x] S1 the side-by-side diff
///       [~] S2 the session picker
///       [ ] S3 the mode card
/// ```
///
/// A heading with no items at all is not "done": an empty section is a section
/// nobody has filled in, and org does not mark it either. It carries no cookie
/// and no box.
pub(crate) fn repo_todos_map(workspace: &str) -> Vec<TodoRow> {
    // **The project directory, not the workspace** — the same resolution the git field uses
    // (`gitfield::project_dir`), so a nested layout (session in `Projects/x`, repo in
    // `Projects/x/x`) loads one tree's TODO.md in the pane and one tree's branch in the header,
    // and never one of each.
    let path = crate::gitfield::project_dir(workspace).join("TODO.md");
    let body = match std::fs::read_to_string(&path) {
        Ok(b) => b,
        Err(e) => {
            return vec![TodoRow {
                indent: 4,
                mark: None,
                text: format!("(no TODO.md in {workspace}: {e})"),
                body: Vec::new(),
                item: false,
            }];
        }
    };
    render_todo_md(&body)
}

/// **The two numbers the todos pane's header draws**, from the one list it draws its rows from.
///
/// `(open, postponed)`. `open` is every row the model still owes — `pending` or `in_progress`,
/// which is exactly the set the idle check may ask about — and `postponed` is every row the
/// operator has set aside. `completed` is neither: it is a record, and a header counting it would
/// be answering a question nobody asks at a glance.
///
/// **A free function over the list, and not a second count kept anywhere.** The defect this exists
/// to prevent is a pane that disagrees with itself — a header derived from the wire while the rows
/// come from the head's own copy, or a total maintained beside the list it counts — and the way to
/// make that impossible is for the count and the rows to be two readings of ONE argument.
/// `todos_lines` passes the same `self.todos` it is about to draw.
pub(crate) fn todo_counts(todos: &[letibot_sessionlog::event::TodoEntry]) -> (usize, usize) {
    let mut open = 0usize;
    let mut postponed = 0usize;
    for t in todos {
        match t.status {
            letibot_sessionlog::event::TodoStatus::Pending
            | letibot_sessionlog::event::TodoStatus::InProgress => open += 1,
            letibot_sessionlog::event::TodoStatus::Postponed => postponed += 1,
            letibot_sessionlog::event::TodoStatus::Completed => {}
        }
    }
    (open, postponed)
}

/// One item's state, in the marks org and `todo_write` share.
/// **A checklist mark** — `[ ]`, `[~]`, `[x]`, `[p]` — rano's, because the pane that paints it
/// is rano's; the reader below parses the file's spellings with its `of`.
pub(crate) use rano::agent::todos::TodoMark;

/// The parsing and rendering, split from the read so it can be tested without a
/// file.
/// One rendered row: how far it is indented, the mark to paint, and the text.
///
/// The indent is carried rather than baked into the text because it belongs
/// BEFORE the mark, and the mark is the part the pane paints — a row that
/// arrived pre-indented painted as `[x]     Phase 0` with the colour in the
/// wrong place entirely. Split so the parse is testable without a palette.
/// One rendered row: how far it is indented, the mark to paint, the text, and
/// the item's own continuation lines when it has any.
///
/// The indent is carried rather than baked into the text because it belongs
/// BEFORE the mark, and the mark is the part the pane paints — a row that
/// arrived pre-indented painted as `[x]     Phase 0` with the colour in the
/// wrong place entirely. Split so the parse is testable without a palette.
pub(crate) struct TodoRow {
    /// Columns before the mark. Carried rather than baked into the text because
    /// it belongs BEFORE the mark, and the mark is the part the pane paints — a
    /// pre-indented row painted as `[x]     Phase 0`, the colour in front of the
    /// whitespace rather than on the box.
    pub(crate) indent: usize,
    /// `None` only for a heading with no checkboxes under it, which org does not
    /// mark either.
    pub(crate) mark: Option<TodoMark>,
    pub(crate) text: String,
    pub(crate) body: Vec<String>,
    /// **A heading carries a mark too** — the roll-up of the rows beneath it —
    /// so the mark cannot be what tells the two apart, and the cursor landed on
    /// headings when it was. There is nothing to unfold on one.
    pub(crate) item: bool,
}

/// One item as the file has it: its mark, its first line, and the continuation
/// lines under it.
///
/// **The body was dropped**, so `T2 vendor yason + alexandria +
/// trivial-gray-streams, pinned in` was the whole of what the pane showed — an
/// item trailing off mid-sentence, with the commits it pins and the `Deps:` line
/// that says what blocks it both gone. The operator, after the items were finally
/// drawn at all: *"if a todo has some associated text? should i be able to expand
/// it somehow?"*
pub(crate) struct TodoItem {
    pub(crate) mark: TodoMark,
    pub(crate) head: String,
    pub(crate) body: Vec<String>,
}

pub(crate) fn render_todo_md(body: &str) -> Vec<TodoRow> {
    let mut out: Vec<TodoRow> = Vec::new();
    let mut section: Option<String> = None;
    let mut items: Vec<TodoItem> = Vec::new();

    let flush = |out: &mut Vec<TodoRow>, section: &Option<String>, items: &[TodoItem]| {
        let Some(name) = section else { return };
        if items.is_empty() {
            // Not `[x]`: an empty section is one nobody has filled in, and org
            // does not mark it done either.
            out.push(TodoRow {
                indent: 6,
                mark: None,
                text: name.clone(),
                body: Vec::new(),
                item: false,
            });
            return;
        }
        let done = items.iter().filter(|i| i.mark == TodoMark::Done).count();
        // **Org's rule for a parent.** Every child done makes the parent done;
        // any child started makes it started; otherwise it is open.
        let roll = if done == items.len() {
            TodoMark::Done
        } else if items.iter().any(|i| i.mark != TodoMark::Open) {
            TodoMark::Doing
        } else {
            TodoMark::Open
        };
        out.push(TodoRow {
            indent: 4,
            mark: Some(roll),
            text: format!("{name}  [{done}/{}]", items.len()),
            body: Vec::new(),
            item: false,
        });
        for i in items {
            out.push(TodoRow {
                indent: 8,
                mark: Some(i.mark),
                text: i.head.clone(),
                body: i.body.clone(),
                item: true,
            });
        }
    };

    // Whether the last item is still collecting continuation lines. A blank line
    // closes it: two items separated by one would otherwise merge, and the prose
    // between a heading and its list would land on whatever came before.
    let mut collecting = false;

    for line in body.lines() {
        // `##` and deeper: `###` is a subsection and its items belong to it, not
        // to the `##` above, which is what org's outline says too.
        if let Some(name) = line
            .strip_prefix("## ")
            .or_else(|| line.strip_prefix("### "))
        {
            flush(&mut out, &section, &items);
            section = Some(name.trim().to_string());
            items.clear();
            collecting = false;
        } else if let Some((mark, text)) = TodoMark::of(line) {
            items.push(TodoItem {
                mark,
                head: strip_markup(text),
                body: Vec::new(),
            });
            collecting = true;
        } else if line.trim().is_empty() {
            collecting = false;
        } else if collecting
            && (line.starts_with(' ') || line.starts_with('\t'))
            && let Some(last) = items.last_mut()
        {
            // An indented line under an item is that item's detail — which is
            // where a TODO.md puts the commit it pins and the `Deps:` that says
            // what blocks it.
            last.body.push(strip_markup(line.trim()));
        } else {
            // Anything at column zero that is not a checkbox ends the item.
            collecting = false;
        }
    }
    flush(&mut out, &section, &items);
    out
}

/// `**T1** git init` → `T1 git init`. The pane has one style for this text and
/// markdown's emphasis markers are noise in it; the backticks go for the same
/// reason. Nothing else is interpreted — this is a reader, not a renderer.
pub(crate) fn strip_markup(text: &str) -> String {
    text.replace("**", "").replace('`', "")
}
