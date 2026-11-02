//! The todos pane: the session's plan and the repository's queue (`TODO.md`), and the reader that
//! turns `TODO.md` into its rows. The pane is drawn by `rano::agent::todos`; the reader, the counts
//! and which row the cursor is on are this head's.

use crate::app::*;
use crate::ui::render::{row_strings, wrap};
use letibot_sessionlog::event::{TodoBy, TodoEntry, TodoNeed, TodoStatus};
use rano::agent::pane;
use rano::agent::text::clean_line;
use rano::agent::todos::{RepoRow, TodoLine, TodosPane};
use rano::render::Line;

impl App {
    pub(crate) fn todos_lines(&mut self, w: usize) -> Vec<String> {
        let (open, postponed) = todo_counts(&self.todos);
        // Through the one enumeration: the row the cursor is on is the stop at `todos_sel` of the
        // same list the keys act on, matched here by identity (the operator's rows by their words,
        // the file's by their place).
        let stops = self.todos_stops();
        let cursor = self.todos_sel.min(stops.len().saturating_sub(1));
        let marked = |want: &TodoStop| stops.get(cursor).is_some_and(|it| it == want);
        // The operator's numbers, counted over the WHOLE board and BEFORE anything is reordered:
        // this is the number `/todo done N` and `/todo rm N` take, so a counter walking the DRAWN
        // order would renumber every row the moment a done one moved into its group.
        let mut numbers = vec![0usize; self.todos.len()];
        let mut mine_at = 0usize;
        for (i, t) in self.todos.iter().enumerate() {
            if t.by == TodoBy::Operator {
                mine_at += 1;
                numbers[i] = mine_at;
            }
        }
        // The order the pane draws is ONE value ([`board_drawn`]), walked here and by
        // [`App::todos_stops`]: the rows the cursor can stop on and the rows on the screen come out
        // of one walk.
        let plan = board_drawn(&self.todos, self.todos_fold_done);
        // Which of the board's rows the pane actually draws - the indices, because a row's place on
        // the board is what its `waits on` link is made of, and the group line is a line this head
        // splices in, not a row.
        let mut drawn: Vec<usize> = Vec::with_capacity(plan.len());
        // Where the `done (N)` line goes: in DRAWN rows, the count above it, and what it stands for.
        // Read back below, when the pane's own rows are known.
        let mut group_after: Option<usize> = None;
        let mut group_done = 0usize;
        let mut lines: Vec<TodoLine> = Vec::with_capacity(plan.len());
        for line in &plan {
            let i = match *line {
                BoardLine::Row(i) => i,
                BoardLine::DoneGroup { done } => {
                    group_after = Some(lines.len());
                    group_done = done;
                    continue;
                }
            };
            let t = &self.todos[i];
            let mine = t.by == TodoBy::Operator;
            drawn.push(i);
            // A board row has no BODY, and the title cap is what makes that livable: rano's
            // `TodoLine` has no field for a detail, so it rides in `content` after ` - ` and is drawn
            // on the same line. A spliced extra line would put every stop below it a row out.
            lines.push(TodoLine {
                mark: match t.status {
                    TodoStatus::Pending => TodoMark::Open,
                    TodoStatus::InProgress => TodoMark::Doing,
                    TodoStatus::Completed => TodoMark::Done,
                    TodoStatus::Postponed => TodoMark::Postponed,
                    // rano's four marks have no fifth: a cancelled row draws in the SET-ASIDE
                    // register, the closest of the four - faint, kept on the board, not asked about.
                    // The row's words, author, place and the header's count are still right.
                    TodoStatus::Cancelled => TodoMark::Postponed,
                },
                content: t.content.clone(),
                mine,
                // The board's number, and not the row's place in what is drawn - see `numbers`
                // above: a done row that moved down into its group kept the number `/todo` takes.
                number: numbers[i],
                // And a wait `wait_on` armed: a `needs` edge on a job or a child, not a `when`. The
                // first edge is the one drawn and the card lists them all; a job is drawn with its
                // slug and a child by its short name, because those words are the link a click opens.
                waits_on: if asks_operator(t) {
                    Some("you".into())
                } else {
                    wait_of(t).map(|w| self.wait_label(&w))
                },
                // The mark follows the STOP, whichever half the row is on: rano draws its mark from
                // this flag alone, so a model's row left at `false` is a stop the cursor walks onto
                // with nothing drawn under it. `todos_stops` is the one enumeration that says which.
                cursor: marked(&if mine {
                    TodoStop::Mine(t.content.clone())
                } else {
                    TodoStop::Theirs(t.content.clone())
                }),
            });
        }
        let pane = TodosPane {
            open,
            postponed,
            on_add: marked(&TodoStop::Add),
            todos: lines,
            // And the file's half, grouped per SECTION by the same one enumeration: a section's
            // items belong to the heading over them, and each heading keeps the file's own cookie
            // (`[2/7]`), which counts the file and not this pane.
            repo: self.repo_todos.as_ref().map(|rows| {
                let folded = self.todos_fold_done;
                let mut out = Vec::with_capacity(rows.len() + 1);
                for line in repo_drawn(rows, folded) {
                    out.push(match line {
                        RepoLine::Heading(i) => RepoRow {
                            indent: rows[i].indent,
                            mark: rows[i].mark,
                            text: rows[i].text.clone(),
                            body: rows[i].body.clone(),
                            item: false,
                            cursor: false,
                            open: false,
                        },
                        // The section's `done (N)` line, drawn by rano as a heading with no mark: a
                        // `RepoRow` with `item: false` and `mark: None` is exactly a faint label at
                        // the section's indent, and rano's own stop record therefore stays true.
                        RepoLine::DoneGroup { done, indent } => RepoRow {
                            indent,
                            mark: None,
                            text: done_group_words(done, folded),
                            body: Vec::new(),
                            item: false,
                            cursor: false,
                            open: false,
                        },
                        RepoLine::Item(i) => {
                            let r = &rows[i];
                            let here = marked(&TodoStop::Repo(i));
                            RepoRow {
                                indent: r.indent,
                                mark: r.mark,
                                text: r.text.clone(),
                                body: r.body.clone(),
                                item: true,
                                cursor: here,
                                open: here && self.repo_open,
                            }
                        }
                    });
                }
                out
            }),
        };
        // Drawn once at full width, and cut to `w` from that - the same cut `content(w)` makes,
        // kept beside the uncut rows because a link's columns are read off the row as rano built it.
        let mut full = pane.content(usize::MAX);
        let group_row = done_group_line(&mut full, group_after, group_done, self.todos_fold_done);
        let content = full.clone().trimmed(w);
        // The pane row each drawn board row landed on - the block under the add control, with the
        // group line taking a row of its own between the two halves. Everything that reads a row's
        // place reads it from here, not from its index on the board.
        let add = full.stop_rows.first().copied().unwrap_or(0);
        let owed = group_after.unwrap_or(drawn.len());
        let placed = placed_rows(add, owed, drawn.len() - owed, group_row);
        let drawn_mine = drawn
            .iter()
            .filter(|i| self.todos[**i].by == TodoBy::Operator)
            .count();
        // The rows the stops landed on, taken as they went out - what the arrows scroll by and a
        // click is tested against. The model's rows are stops now and rano does not record them, so
        // the record is completed here; see [`session_stop_rows`].
        self.todos_stop_rows = session_stop_rows(&content, &placed, drawn_mine);
        self.todos_wait_links = self.wait_links(&full, &content, w, &drawn, &placed);
        // The window follows the cursor on the frame that MOVED it without a row number to move it
        // by - the switch that folded or opened the done rows under it. Which pane row it is on is
        // what the line above just recorded.
        if std::mem::take(&mut self.todos_follow) {
            let at = self
                .todos_sel
                .min(self.todos_stop_rows.len().saturating_sub(1));
            if let Some(row) = self.todos_stop_rows.get(at).copied() {
                self.scroll_into_view(row);
            }
        }
        row_strings(&content.lines, self.cfg.palette())
    }

    /// Where each row's `waits on ...` words landed, as links a click opens. The row is read off what
    /// rano drew - its last run must be exactly the words this head handed it, or no link is recorded
    /// for it - so a rano that drew the row another way loses the link rather than moving it.
    fn wait_links(
        &self,
        full: &rano::agent::pane::PaneLines,
        cut: &rano::agent::pane::PaneLines,
        w: usize,
        drawn: &[usize],
        placed: &[usize],
    ) -> Vec<WaitLink> {
        let mut out = Vec::new();
        for (pos, (i, row)) in drawn.iter().zip(placed).enumerate() {
            let (i, row) = (*i, *row);
            let t = &self.todos[i];
            if asks_operator(t) {
                continue;
            }
            let Some(target) = wait_of(t) else {
                continue;
            };
            let (Some(line), Some(shown)) = (full.lines.get(row), cut.lines.get(row)) else {
                continue;
            };
            let Some(last) = line.spans.last() else {
                continue;
            };
            let drawn_row = format!(" · waits on {}", clean_line(&self.wait_label(&target)));
            if last.content != drawn_row || !line.plain().contains(&clean_line(&t.content)) {
                continue;
            }
            let end = line.width();
            let from = end - last.width() + " · ".chars().count();
            let visible = if end > w {
                shown.width().saturating_sub(1)
            } else {
                end
            };
            let to = end.min(visible);
            if to > from {
                out.push(WaitLink {
                    row,
                    from,
                    to,
                    // Its place among the rows DRAWN, which is what `1 + todo` of the stops means -
                    // not its index on the board.
                    todo: pos,
                    target,
                });
            }
        }
        out
    }

    /// One row, whole: the overlay the todos pane's Enter opens, the one place on the screen where a
    /// row is not cut to the pane's width. The row is looked up by identity at DRAW time and never
    /// carried here, so a `TodosUpdated` under an open overlay draws the new status.
    pub(crate) fn todos_open_lines(&self, w: usize) -> Vec<String> {
        let Some(stop) = self.todos_open.as_ref() else {
            return Vec::new();
        };
        let mut lines = vec![pane::title("todos — one row"), Line::default()];
        match stop {
            // Reachable by no key, and said rather than drawn empty: Enter on the control opens the
            // card, because the control is not a row with words of its own (`key_todos_pane`).
            TodoStop::Add => lines.push(Line::raw(
                "  the `[+]` control has no text of its own — enter there opens the card.",
            )),
            // The operator's half and the model's half, by the same words and the same rule: the
            // lookup takes the first row with those words on that half, in the list's own order.
            TodoStop::Mine(words) | TodoStop::Theirs(words) => {
                let mine = matches!(stop, TodoStop::Mine(_));
                let held = self
                    .todos
                    .iter()
                    .find(|t| t.content == *words && (t.by == TodoBy::Operator) == mine);
                match held {
                    Some(t) => {
                        lines.push(pane::faint(format!("    {}", self.row_facts(t))));
                        if !t.needs.is_empty() {
                            let needs: Vec<String> =
                                t.needs.iter().map(|n| self.need_words(n)).collect();
                            lines.push(pane::faint(format!("    needs {}", needs.join(", "))));
                        }
                        lines.push(Line::default());
                        push_whole(&mut lines, &t.content, w);
                    }
                    None => lines.push(Line::raw(format!(
                        "  `{}` is not on the board any more — a row's words are its identity, so a \
                         row renamed is a different row. esc goes back to the list.",
                        clean_line(words)
                    ))),
                }
            }
            TodoStop::Repo(i) => match self.repo_todos.as_ref().and_then(|rows| rows.get(*i)) {
                Some(r) if r.item => {
                    lines.push(pane::faint(format!(
                        "    the repo's TODO.md · {}",
                        mark_words(r.mark)
                    )));
                    lines.push(Line::default());
                    push_whole(&mut lines, &r.text, w);
                    if !r.body.is_empty() {
                        lines.push(Line::default());
                        for b in &r.body {
                            push_whole(&mut lines, b, w);
                        }
                    }
                }
                _ => lines.push(Line::raw(format!(
                    "  row {i} of TODO.md is not there any more — the file is read again on every \
                     draw, so an edit under this overlay moves what that row number points at. esc \
                     goes back to the list."
                ))),
            },
        }
        lines.push(Line::default());
        lines.push(pane::faint("    esc goes back to the list"));
        row_strings(&lines, self.cfg.palette())
    }
}

/// A row's own words, whole, wrapped and indented - the one place in this pane where a row is not
/// cut to the pane's width. Split on newlines FIRST and cleaned line by line, because `clean_line`
/// turns a newline into a space and the shape the author wrote would be lost.
fn push_whole(out: &mut Vec<Line>, text: &str, w: usize) {
    for l in text.split('\n') {
        let rows = wrap(&clean_line(l), w.saturating_sub(2).max(1));
        if rows.is_empty() {
            out.push(Line::default());
        }
        for one in rows {
            out.push(Line::raw(format!("  {one}")));
        }
    }
}

impl App {
    /// Who wrote a row and where it stands - the overlay's own header line, and the two facts the
    /// pane's row carries in its margin (`- model`, the mark). A `when` job is said as the pane says
    /// it, with its slug: `waits on j97 - pg-timing`.
    fn row_facts(&self, t: &TodoEntry) -> String {
        let mut s = format!("{} · {}", author_words(&t.by), status_words(t.status));
        if let Some(letibot_sessionlog::event::TodoCondition::Job { handle }) = &t.when {
            let label = self.wait_label(&WaitOn::Job(handle.clone()));
            s.push_str(&format!(" · waits on {}", clean_line(&label)));
        }
        s
    }

    /// One edge of `needs`, as the sentence a reader can act on. A job carries its slug as the pane
    /// draws it; a child its short name and its whole id, because this view is where the id the pane
    /// shortened is still to be read.
    fn need_words(&self, n: &TodoNeed) -> String {
        match n {
            TodoNeed::Job { handle } => {
                format!(
                    "job {}",
                    clean_line(&self.wait_label(&WaitOn::Job(handle.clone())))
                )
            }
            TodoNeed::Child { id } => {
                let label = self.wait_label(&WaitOn::Child(id.clone()));
                if label == *id {
                    format!("child {}", clean_line(id))
                } else {
                    format!("child {} ({})", clean_line(&label), clean_line(id))
                }
            }
            other => need_words(other),
        }
    }
}

/// The row waits on the OPERATOR - an open row with a question edge. It draws `waits on you` and its
/// act is the question card, not a link.
fn asks_operator(t: &TodoEntry) -> bool {
    !matches!(t.status, TodoStatus::Completed | TodoStatus::Cancelled)
        && t.needs
            .iter()
            .any(|n| matches!(n, TodoNeed::Operator { .. }))
}

/// The author, in the pane's own words - `you` and `the model`, one row apart. A parent's row shows
/// the string it arrived with, because re-spelling it here would be a second name for one fact.
fn author_words(by: &TodoBy) -> String {
    match by {
        TodoBy::Operator => "you".into(),
        TodoBy::Model => "the model".into(),
        TodoBy::Parent(who) => clean_line(who),
        TodoBy::Queue => "the merge queue".into(),
    }
}

/// The row's state as a reader's word. The pane draws rano's four marks (`[ ]`, `[~]`, `[x]`,
/// `[p]`); this is the same four said out, plus the fifth state the marks have no glyph for.
fn status_words(s: TodoStatus) -> &'static str {
    match s {
        TodoStatus::Pending => "open",
        TodoStatus::InProgress => "in progress",
        TodoStatus::Completed => "done",
        TodoStatus::Postponed => "set aside",
        TodoStatus::Cancelled => "cancelled",
    }
}

/// The options of an operator question, on one line - read from the structured list, each quoted, so
/// an option that holds a separator stays one option (the transcript lists them one per line; this
/// line cannot).
fn options_words(options: &[String]) -> String {
    options
        .iter()
        .map(|o| format!("\"{}\"", clean_line(o)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// One edge of `needs`, as the sentence a reader can act on.
fn need_words(n: &TodoNeed) -> String {
    match n {
        TodoNeed::Row { content } => format!("row `{}`", clean_line(content)),
        TodoNeed::Child { id } => format!("child {}", clean_line(id)),
        TodoNeed::Job { handle } => format!("job {}", clean_line(handle)),
        TodoNeed::Monitor { name } => format!("monitor {}", clean_line(name)),
        // The room's item, by the number the `room` tool takes: the edge the daemon's room pass puts
        // on a `you hold ...` row while the room is working on it, so the row is quiet on purpose and
        // a person can see why.
        TodoNeed::Room { item, working } => format!(
            "room item #{item}{}",
            if *working {
                " (the room is working on it)"
            } else {
                " (yours to finish)"
            }
        ),
        TodoNeed::Operator { question, options } => format!(
            "YOU — \"{}\" ({}) — enter on the row answers it",
            clean_line(question),
            options_words(options)
        ),
        TodoNeed::Answered { question, reply } => format!(
            "answered — \"{}\": {}",
            clean_line(question),
            clean_line(reply)
        ),
        // Kept rather than dropped - a plan is what would be lost - and said as the one thing this
        // build can honestly say about it.
        TodoNeed::Unknown => "an edge this build does not know".into(),
    }
}

/// A `TODO.md` item's mark as a word, for the overlay's header line.
fn mark_words(m: Option<TodoMark>) -> &'static str {
    match m {
        Some(TodoMark::Open) => "open",
        Some(TodoMark::Doing) => "in progress",
        Some(TodoMark::Done) => "done",
        Some(TodoMark::Postponed) => "set aside",
        // A heading carries no mark; a heading is not a stop, so this is the honest answer rather
        // than a guess at a state the file never gave.
        None => "unmarked",
    }
}

/// The pane row each drawn board row landed on - the session block under the add control, in the
/// order [`board_drawn`] gave it, with the `done (N)` line taking a row of its own. One function,
/// because the stop record and a `waits on` link's columns need the same answer.
fn placed_rows(add: usize, owed: usize, done: usize, group: bool) -> Vec<usize> {
    let base = add + 1;
    let done_from = base + owed + usize::from(group);
    (0..owed)
        .map(|k| base + k)
        .chain((0..done).map(|k| done_from + k))
        .collect()
}

/// Every stop's drawn row, with the session's rows put back in: rano records a stop only for the rows
/// its own pane acts on, and this head's cursor reaches a model's row too, so without this the record
/// and the enumeration are two lists of different lengths. A record the layout does not fit is kept.
fn session_stop_rows(
    content: &rano::agent::pane::PaneLines,
    placed: &[usize],
    mine: usize,
) -> Vec<usize> {
    let recorded = &content.stop_rows;
    // The recorded stops are the control, the operator's rows, then the repo's items.
    let repo_from = 1 + mine;
    let Some(&add) = recorded.first() else {
        return recorded.clone();
    };
    if recorded.len() < repo_from || placed.last().is_some_and(|r| *r >= content.lines.len()) {
        return recorded.clone();
    }
    let mut out = Vec::with_capacity(recorded.len() - mine + placed.len());
    out.push(add);
    out.extend_from_slice(placed);
    out.extend_from_slice(&recorded[repo_from..]);
    out
}

/// The one muted line that names the done group - `done (3)`, and `done (3) - d shows them` while it
/// is folded - placed where the group starts, so a reader who wonders where the rows went reads the
/// answer where they miss them. Returns whether a row was INSERTED: the rows below move down one.
fn done_group_line(
    full: &mut rano::agent::pane::PaneLines,
    group_after: Option<usize>,
    done: usize,
    folded: bool,
) -> bool {
    let Some(owed) = group_after else {
        return false;
    };
    let Some(add) = full.stop_rows.first().copied() else {
        return false;
    };
    let at = add + 1 + owed;
    let line = pane::faint(format!("    {}", done_group_words(done, folded)));
    let none_yet = full
        .lines
        .get(at)
        .is_some_and(|l| l.plain().trim_start().starts_with("none yet"));
    if none_yet {
        full.lines[at] = line;
        false
    } else {
        insert_row(full, at, line);
        true
    }
}

/// The words the done group is drawn as, on both halves of the board - one line for a reader to
/// learn, and the same one open or shut so that folding it does not read as a different thing.
fn done_group_words(done: usize, folded: bool) -> String {
    if folded {
        format!("done ({done}) · d shows them")
    } else {
        format!("done ({done})")
    }
}

/// A row spliced into a pane this head did not draw, with every stop the pane recorded at or below it
/// moved down one: the drawing and the record are one value, so inserting without moving the record
/// puts every stop below it on its neighbour's line.
fn insert_row(content: &mut rano::agent::pane::PaneLines, at: usize, line: Line) {
    let at = at.min(content.lines.len());
    content.lines.insert(at, line);
    for row in &mut content.stop_rows {
        if *row >= at {
            *row += 1;
        }
    }
}

/// The repo's `TODO.md` as a section map with its items under each heading: a parent is DONE when
/// every child is, and org's own cookie - `[2/7]` - says how far along the rest are. Read fresh on
/// every pane-open, because the file is the operator's to edit and a cached map is somebody else's.
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
/// A heading with no items at all is not "done": an empty section is a section nobody has filled in,
/// and org does not mark it either. It carries no cookie and no box.
pub(crate) fn repo_todos_map(workspace: &str) -> Vec<TodoRow> {
    // The list's own resolution, not the repository's: `gitfield::todos_path` prefers the workspace's
    // `TODO.md` and falls back to the project's, so a session in `Projects/x` whose repository is
    // `Projects/x/x` opens the folder's list when the operator keeps it there.
    let path = crate::gitfield::todos_path(workspace);
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

/// The two numbers the todos pane's header draws, from the one list it draws its rows from:
/// `(open, postponed)`. `completed` and `cancelled` are neither - they are a record, not a question
/// anybody asks at a glance - and one function over the list is what keeps the pane from disagreeing.
pub(crate) fn todo_counts(todos: &[letibot_sessionlog::event::TodoEntry]) -> (usize, usize) {
    let mut open = 0usize;
    let mut postponed = 0usize;
    for t in todos {
        match t.status {
            letibot_sessionlog::event::TodoStatus::Pending
            | letibot_sessionlog::event::TodoStatus::InProgress => open += 1,
            letibot_sessionlog::event::TodoStatus::Postponed => postponed += 1,
            letibot_sessionlog::event::TodoStatus::Completed
            | letibot_sessionlog::event::TodoStatus::Cancelled => {}
        }
    }
    (open, postponed)
}

/// One item's state, in the marks org and `todo_write` share. A checklist mark - `[ ]`, `[~]`, `[x]`,
/// `[p]` - is rano's, because the pane that paints it is rano's; the reader below parses the file's
/// spellings with its `of`.
pub(crate) use rano::agent::todos::TodoMark;

/// One rendered row: how far it is indented, the mark to paint, the text, and the item's own
/// continuation lines when it has any. The indent is carried rather than baked into the text because
/// it belongs BEFORE the mark, which is the part the pane paints.
pub(crate) struct TodoRow {
    /// Columns before the mark. Carried rather than baked into the text because it belongs BEFORE the
    /// mark, and the mark is the part the pane paints - a pre-indented row would put the colour in
    /// front of the whitespace rather than on the box.
    pub(crate) indent: usize,
    /// `None` only for a heading with no checkboxes under it, which org does not
    /// mark either.
    pub(crate) mark: Option<TodoMark>,
    pub(crate) text: String,
    pub(crate) body: Vec<String>,
    /// A heading carries a mark too - the roll-up of the rows beneath it - so the mark cannot be what
    /// tells the two apart, and the cursor landed on headings when it was. There is nothing to unfold
    /// on one.
    pub(crate) item: bool,
}

/// One item as the file has it: its mark, its first line, and the continuation lines under it. The
/// body is here because without it an item trails off mid-sentence, with the commits it pins and the
/// `Deps:` line that says what blocks it both gone.
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
        // Org's rule for a parent. Every child done makes the parent done; any child started makes it
        // started; otherwise it is open.
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

#[cfg(test)]
mod options_tests {
    /// R90.24: an option holding a separator stays one option on the pane's line.
    #[test]
    fn options_are_quoted_one_by_one() {
        let o = vec!["a / b".to_string(), "c | d".to_string()];
        assert_eq!(super::options_words(&o), "\"a / b\", \"c | d\"");
    }
}
